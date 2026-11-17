#!/usr/bin/env bash
#
# Local integration test for the macrdp security AUDIT LOG writes.
#
# Drives two real CredSSP/NLA connections with sdl-freerdp (`+auth-only`, so no
# GUI window — it runs only the authentication exchange and exits) against a
# loopback macrdp instance, then asserts the JSON audit stream
# (--audit-file) recorded the right accept / auth / disconnect events with the
# correct outcomes, fields, and (src_ip, src_port) correlation.
#
# It needs NO real Mac password: --skip-auth makes macrdp validate CredSSP
# against a throwaway --username/--password, and loopback is guard-exempt so the
# wrong-password attempt can't trip a lockout. See docs/audit-log.md.
#
# Requirements: sdl-freerdp (brew install freerdp), a macrdp build (auto-built),
# python3 and lsof (included with macOS). Loopback only. Exit 0 = PASS.
#
# Usage:  scripts/test-audit-log.sh            # port 3399
#         MACRDP_TEST_PORT=4000 scripts/test-audit-log.sh
#         MACRDP_TEST_BINARY=target/debug/macrdp scripts/test-audit-log.sh
set -u

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${MACRDP_TEST_PORT:-3399}"
USER_NAME="audittest"
GOOD_PW="s3cret-good"
BAD_PW="s3cret-WRONG"
WORK="$(mktemp -d)"
AUDIT="$WORK/audit.jsonl"
SRVLOG="$WORK/macrdp.log"

FRDP="$(command -v sdl-freerdp || command -v sdl3-freerdp || true)"
[[ -z "$FRDP" ]] && { echo "!! sdl-freerdp not found (brew install freerdp)"; exit 2; }
LSOF="$(command -v lsof || true)"
[[ -z "$LSOF" ]] && { echo "!! lsof not found"; exit 2; }

# Pick an existing build that actually supports --audit-file (a stale binary
# from before the feature would fail); otherwise build debug.
supports_audit() { "$1" --help 2>/dev/null | grep -q -- '--audit-file'; }
BIN="${MACRDP_TEST_BINARY:-}"
if [[ -n "$BIN" ]]; then
  [[ -x "$BIN" ]] && supports_audit "$BIN" || { echo "!! explicit test binary does not support audit logging"; exit 2; }
else
  for cand in "$REPO/target/release/macrdp" "$REPO/target/debug/macrdp"; do
    [[ -x "$cand" ]] && supports_audit "$cand" && { BIN="$cand"; break; }
  done
fi
[[ -z "$BIN" ]] && { echo "== building macrdp (debug) =="; (cd "$REPO" && cargo build) || exit 2; BIN="$REPO/target/debug/macrdp"; }

cleanup() { [[ -n "${SRV_PID:-}" ]] && kill "$SRV_PID" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"; }
trap cleanup EXIT

# Keep each client's exit status and audit events together. FreeRDP SDL 3.33.1
# can exit 1 after successful auth: sdl_run returns 1, and main only replaces it
# with the auth thread's exit code when that code is nonzero. Therefore require
# an explicit SDL auth result AND the matching server verdict, not exit alone.
# https://github.com/FreeRDP/FreeRDP/blob/3.33.1/client/SDL/SDL3/sdl_freerdp.cpp#L753-L761
# In particular, 137 is also FreeRDP's CONNECT_UNDEFINED, so a numeric exit
# code alone must never be used to infer that our timeout fired.
run_phase() {
  local phase=$1; shift
  python3 - "$AUDIT" "$WORK/$phase.json" "$WORK/$phase.log" "$@" <<'PY'
import json, pathlib, re, subprocess, sys, time

audit, result_path, log_path = map(pathlib.Path, sys.argv[1:4])

def read_rows():
    try:
        data = audit.read_bytes()
    except FileNotFoundError:
        return []
    # The asynchronous writer may still be writing the final JSON line.
    return [json.loads(line) for line in data.splitlines(keepends=True)
            if line.endswith(b"\n") and line.strip()]

def peer(row):
    return (row.get("src_ip"), row.get("src_port"))

start = len(read_rows())
result = {"start": start, "exit_code": None, "timed_out": False}
with log_path.open("w") as log:
    try:
        result["exit_code"] = subprocess.run(
            sys.argv[4:], stdout=log, stderr=subprocess.STDOUT, timeout=20,
            check=False).returncode
    except subprocess.TimeoutExpired:
        # subprocess.run kills and reaps the child before raising.
        result["timed_out"] = True
    except OSError as exc:
        result["launch_error"] = str(exc)

# The core's "exit status 1" is a BOOL, not the process exit status. The SDL
# summary names the actual authentication result before intentional teardown.
client_log = log_path.read_text(errors="replace")
result["client_auth"] = [(name, int(code, 16)) for name, code in re.findall(
    r"\[sdl_client_thread_connect\]: Authentication only, (\w+) \[(0x[0-9a-fA-F]{8})\]",
    client_log)]
result["client_reported_success"] = bool(re.search(
    r"Authentication only, (?:ERRBASE_SUCCESS \[0x00000000\]|exit status 1\s*$)",
    client_log, re.MULTILINE))

# Do not let the previous client's delayed disconnect/auth events spill into
# the next phase. Bound this wait; a missing audit event must fail the checks.
deadline = time.monotonic() + 5
while True:
    rows = read_rows()[start:]
    accepts = {peer(row) for row in rows if row.get("event") == "accept"}
    auths = {peer(row) for row in rows if row.get("event") == "auth"}
    disconnects = {peer(row) for row in rows if row.get("event") == "disconnect"}
    drained = bool(accepts) and accepts <= auths and accepts <= disconnects
    if drained or time.monotonic() >= deadline:
        break
    time.sleep(0.05)

result.update(rows=rows, end=start + len(rows), audit_drained=drained)
result_path.write_text(json.dumps(result))
print("   exit_code={exit_code}, timed_out={timed_out}, audit_drained={audit_drained}".format(**result))
PY
}

echo "== starting macrdp (loopback :$PORT, --skip-auth, audit -> audit.jsonl) =="
MACRDP_HEALTHCHECK=0 "$BIN" \
  --bind "127.0.0.1:$PORT" --skip-auth --allow-sleep \
  --username "$USER_NAME" --password "$GOOD_PW" \
  --audit-file "$AUDIT" --cert-dir "$WORK/certs" \
  >"$SRVLOG" 2>&1 &
SRV_PID=$!

# Query this process's listener without creating extra accept/disconnect events.
# A different process already using PORT cannot satisfy this readiness check.
listening() { "$LSOF" -nP -a -p "$SRV_PID" -iTCP@127.0.0.1:"$PORT" -sTCP:LISTEN -t >/dev/null 2>&1; }
for _ in $(seq 1 40); do
  listening && break
  kill -0 "$SRV_PID" 2>/dev/null || { echo "!! macrdp exited early:"; cat "$SRVLOG"; exit 1; }
  sleep 0.25
done
listening || { echo "!! macrdp never listened:"; cat "$SRVLOG"; exit 1; }
echo "   listening (pid $SRV_PID)"

echo "== connection 1: CORRECT password (expect auth success) =="
run_phase good "$FRDP" "/v:127.0.0.1:$PORT" "/u:$USER_NAME" "/p:$GOOD_PW" /cert:ignore +auth-only /log-level:ERROR || exit 1
echo "   done"

echo "== connection 2: WRONG password (expect auth did_not_complete) =="
run_phase bad "$FRDP" "/v:127.0.0.1:$PORT" "/u:$USER_NAME" "/p:$BAD_PW" /cert:ignore +auth-only /log-level:ERROR || exit 1
echo "   done"

echo
echo "== audit events written =="
cat "$AUDIT"
echo
echo "== assertions =="
python3 - "$AUDIT" "$WORK/good.json" "$WORK/bad.json" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
good, bad = [json.load(open(path)) for path in sys.argv[2:]]
def ev(**kw): return [r for r in rows if all(r.get(k) == v for k, v in kw.items())]

fails = []
def check(name, ok):
    print(("  PASS " if ok else "  FAIL ") + name)
    if not ok: fails.append(name)

check("audit file is non-empty", bool(rows))
check("every row has schema_version=1 + event + src_ip",
      all(r.get("schema_version") == 1 and "event" in r and "src_ip" in r for r in rows))
check("target is macrdp::audit on every row", all(r.get("target") == "macrdp::audit" for r in rows))
check("audit phases are contiguous and cover the complete log",
      good["start"] == 0 and good["end"] == bad["start"]
      and bad["end"] == len(rows)
      and good["rows"] + bad["rows"] == rows)

def tup(r): return (r.get("src_ip"), r.get("src_port"))

for name, phase, outcome in [("correct password", good, "success"),
                             ("wrong password", bad, "did_not_complete")]:
    code = phase["exit_code"]
    check(name + ": client ran without a timeout or launch error",
          not phase["timed_out"] and "launch_error" not in phase)
    # Accept only documented SDL success/outer-loop codes or AUTH_FAILURE=132.
    # Negative subprocess return codes mean signal termination, not rejection.
    check(name + ": client exit status matches the expected verdict",
          code in (0, 1) if outcome == "success" else code == 132)
    expected_client = ("ERRBASE_SUCCESS", 0) if outcome == "success" else (
        "ERRCONNECT_AUTHENTICATION_FAILED", 0x00020009)
    check(name + ": explicit client auth result matches the server verdict",
          bool(phase["client_auth"])
          and all(tuple(verdict) == expected_client for verdict in phase["client_auth"]))
    if outcome != "success":
        check(name + ": client reported no authentication success",
              not phase["client_reported_success"])
    check(name + ": all accepted connections completed their audit lifecycle",
          phase["audit_drained"])
    phase_rows = phase["rows"]
    auths = [r for r in phase_rows if r.get("event") == "auth"]
    accepts = [r for r in phase_rows if r.get("event") == "accept"]
    disconnects = [r for r in phase_rows if r.get("event") == "disconnect"]
    check(name + ": every auth event has the expected outcome",
          bool(auths) and all(r.get("outcome") == outcome for r in auths))
    check(name + ": every auth correlates to its own phase's accept AND disconnect",
          bool(auths) and all(
              sum(tup(a) == tup(r) for a in accepts) == 1
              and sum(tup(d) == tup(r) for d in disconnects) == 1
              for r in auths))
    check(name + ": exactly one auth verdict per accepted connection",
          bool(accepts) and all(sum(tup(a) == tup(r) for r in auths) == 1 for a in accepts))

acc   = ev(event="accept")
au_ok = ev(event="auth", outcome="success")
au_no = ev(event="auth", outcome="did_not_complete")
dis   = ev(event="disconnect")

check(">=2 accept events (one per connection)", len(acc) >= 2)
check(">=1 auth did_not_complete WITH a reason",
      len(au_no) >= 1 and all(r.get("reason") for r in au_no))
check("auth success is INFO, auth failure is WARN",
      all(r.get("level") == "INFO" for r in au_ok) and all(r.get("level") == "WARN" for r in au_no))
check("auth events carry src_ip + src_port",
      all("src_ip" in r and "src_port" in r for r in au_ok + au_no))
check("reject events (if any) carry src_ip but NOT src_port",
      all(("src_ip" in r and "src_port" not in r) for r in ev(event="reject")))
check(">=2 disconnect events", len(dis) >= 2)

check("auth-failure reason is a clean single-line token (no control chars)",
      all(not any(ord(c) < 0x20 or ord(c) == 0x7f for c in (r.get("reason") or "")) for r in au_no))

print()
if fails:
    print(f"RESULT: FAIL ({len(fails)} check(s) failed)"); sys.exit(1)
print("RESULT: PASS — audit log writes verified end-to-end"); sys.exit(0)
PY
rc=$?
echo
[[ $rc -eq 0 ]] && echo "OVERALL: PASS" || echo "OVERALL: FAIL"
if [[ $rc -ne 0 ]]; then
  echo "== correct-password client log =="
  cat "$WORK/good.log"
  echo "== wrong-password client log =="
  cat "$WORK/bad.log"
fi
exit $rc
