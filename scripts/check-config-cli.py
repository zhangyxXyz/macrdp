#!/usr/bin/env python3
"""Process-level inspection smoke test; never starts an RDP session.

Usage: python3 scripts/check-config-cli.py target/release/macrdp
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile


binary = Path(sys.argv[1]).resolve()
checks = 0


def invoke(*args, expected=0):
    global checks
    result = subprocess.run(
        [str(binary), *map(str, args)], capture_output=True, text=True, timeout=5
    )
    assert result.returncode == expected, (args[0], result.returncode)
    assert not result.stderr, "inspection unexpectedly wrote diagnostics outside JSON"
    report = json.loads(result.stdout)
    checks += 1
    return report


caps = invoke("--capabilities")
assert caps["schema_version"] == 1
assert {"config_validation", "display_state_v2"} <= set(caps["features"])
assert {"RESOLUTION_POLICY", "LIVE_RESIZE", "VD_HIDPI"} <= set(caps["supported_config_keys"])

with tempfile.TemporaryDirectory(prefix="macrdp-config-smoke-") as temporary:
    directory = Path(temporary)
    candidate = directory / "candidate.env"
    base = "VIRTUAL_DISPLAY=1\nVD_WIDTH=3840\nVD_HEIGHT=2160\nVD_HIDPI=1\n"

    def validate(text, valid=True):
        candidate.write_text(text)
        report = invoke("--check-config", candidate, expected=0 if valid else 2)
        assert report["ok"] is valid
        if valid:
            assert report["config_revision"].startswith("sha256:")
        return report

    fixed = validate(base + "RESOLUTION_POLICY=fixed\nLIVE_RESIZE=0\n")
    assert fixed["effective"]["resolution_policy"] == "fixed"
    assert fixed["effective"]["live_resize"] is False
    assert fixed["effective"]["vd_width"] == 3840
    assert fixed["effective"]["vd_hidpi"] == "1"

    follow = validate("export\t VIRTUAL_DISPLAY='1'\nVD_WIDTH=3840\nVD_HEIGHT=2160\n"
                      'VD_HIDPI="auto"\nRESOLUTION_POLICY=client\nUNKNOWN_FUTURE_KEY=keep\n')
    assert follow["effective"]["vd_hidpi"] == "auto"
    assert follow["warnings"]
    legacy = validate(base + 'EXTRA_FLAGS="\'--no-client-resolution\'"\n')
    assert legacy["effective"]["resolution_policy"] == "fixed"

    for invalid in [
        base + 'RESOLUTION_POLICY=client\nEXTRA_FLAGS="--no-client-resolution"\n',
        base + "VD_WIDTH=1920\n",
        base.replace("3840", "0"),
        base.replace("VIRTUAL_DISPLAY=1", "VIRTUAL_DISPLAY=yes"),
        base + "LIVE_RESIZE=maybe\n",
        "TLS_CERT=/not-read-by-validation\n",
        'EXTRA_FLAGS="--help"\n',
    ]:
        validate(invalid, valid=False)

    # Neither referenced credential files nor output paths should be touched.
    validate("USE_KEYCHAIN=0\nPASSWORD_FILE=/file-that-does-not-exist/macrdp\n")
    secret = "synthetic-inspection-secret"
    report = validate(f'USE_KEYCHAIN=0\nEXTRA_FLAGS="--password {secret} '
                      f'--log-dir {directory / "must-not-exist"}"\n')
    assert secret not in json.dumps(report)
    assert set(directory.iterdir()) == {candidate}, "inspection created runtime files"
    invoke("--check-config", directory / "missing.env", expected=2)

print(f"Config CLI: {checks} process-level checks passed; no runtime files created")
