#!/usr/bin/env bash
# Exercise the vendored RSA adapters, whose own tests are not run by root
# `cargo test`. No server, real credentials, smart card or macOS permissions.
# Requires Cargo and Python 3.9+. Run without other standalone Cargo commands
# in vendor/picky or vendor/winscard: their temporary locks are restored on exit.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
export CARGO_TARGET_DIR="$REPO/target"

cleanup() {
  local status=$?
  local restored=1
  trap - EXIT
  for package in picky winscard; do
    if [[ -f "$WORK/$package.lock" ]]; then
      cp -p "$WORK/$package.lock" "$REPO/vendor/$package/Cargo.lock" || restored=0
    elif [[ -f "$WORK/$package.absent" ]]; then
      rm -f "$REPO/vendor/$package/Cargo.lock" || restored=0
    fi
  done
  if [[ $restored -eq 1 ]]; then
    rm -rf "$WORK"
  else
    echo "!! Could not restore standalone locks; backups remain in $WORK" >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Back up both locks before seeding either. The root lock is only read; refuse
# symlink destinations so a standalone lock cannot alias another lock file.
cp "$REPO/Cargo.lock" "$WORK/root.lock"
for package in picky winscard; do
  lock="$REPO/vendor/$package/Cargo.lock"
  if [[ -L "$lock" ]]; then
    echo "!! Refusing a symlinked standalone lock: $lock" >&2
    exit 1
  elif [[ -e "$lock" ]]; then
    cp -p "$lock" "$WORK/$package.lock.tmp"
    mv "$WORK/$package.lock.tmp" "$WORK/$package.lock"
  else
    touch "$WORK/$package.absent"
  fi
done
for package in picky winscard; do
  cp "$WORK/root.lock" "$REPO/vendor/$package/Cargo.lock"
done

# Dev dependencies extend the seed, so --locked is intentionally not used.
# They must not select a different production crypto version. Parse only the
# name/version fields in Cargo's generated package tables (no tomllib needed).
check_crypto_versions() {
  python3 - "$WORK/root.lock" "$REPO/vendor/$1/Cargo.lock" <<'PY'
import pathlib, re, sys

critical = ("rsa", "crypto-bigint", "ctutils", "der", "spki", "pkcs1")

def versions(path):
    result = {name: set() for name in critical}
    for package in pathlib.Path(path).read_text().split("[[package]]")[1:]:
        name = re.search(r'^name = "([^"]+)"$', package, re.MULTILINE)
        version = re.search(r'^version = "([^"]+)"$', package, re.MULTILINE)
        if name and version and name[1] in result:
            result[name[1]].add(version[1])
    return result

root, tested = map(versions, sys.argv[1:])
for name in critical:
    if not root[name] or not tested[name] or not tested[name] <= root[name]:
        raise SystemExit("crypto lock mismatch for {}: root={}, tested={}".format(
            name, sorted(root[name]), sorted(tested[name])))
print("PASS: tested crypto versions match the root lock: " + ", ".join(
    "{}={}".format(name, "/".join(sorted(tested[name]))) for name in critical))
PY
}

echo "== picky: default library tests =="
cargo test --manifest-path "$REPO/vendor/picky/Cargo.toml" --lib
check_crypto_versions picky

echo "== picky: full SSH/PuTTY key and JWE regressions =="
cargo test --manifest-path "$REPO/vendor/picky/Cargo.toml" --lib --features ssh,putty
check_crypto_versions picky

# Cargo patches belong to the standalone root; explicitly select our picky
# adapter when testing winscard instead of resolving its registry counterpart.
PICKY_PATCH="$(python3 -c 'import json, sys; print("patch.crates-io.picky.path=" + json.dumps(sys.argv[1]))' "$REPO/vendor/picky")"
echo "== winscard: simulated smart-card library tests =="
cargo test --manifest-path "$REPO/vendor/winscard/Cargo.toml" --config "$PICKY_PATCH" --lib
check_crypto_versions winscard

cmp -s "$WORK/root.lock" "$REPO/Cargo.lock" || {
  echo "!! Root Cargo.lock changed while running dependency regressions" >&2
  exit 1
}
echo "PASS: auth dependency regressions; root Cargo.lock unchanged"
