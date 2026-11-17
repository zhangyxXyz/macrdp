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
assert {"RESOLUTION_POLICY", "LIVE_RESIZE", "VD_HIDPI", "CAPTURE_NV12", "AUDIO_REMOTE_ONLY", "H264_MAX_UNACKED_FRAMES"} <= set(caps["supported_config_keys"])

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

    assert "DISPLAY_WAKE_TIMEOUT_SECS" in caps["supported_config_keys"]
    assert fixed["effective"]["display_wake_timeout_secs"] == 3
    for value in [1, 3, 15]:
        wake = validate(f"DISPLAY_WAKE_TIMEOUT_SECS={value}\n")
        assert wake["effective"]["display_wake_timeout_secs"] == value
    for value in ["0", "16", "3.5", "never"]:
        validate(f"DISPLAY_WAKE_TIMEOUT_SECS={value}\n", valid=False)
    via_flag = validate('DISPLAY_WAKE_TIMEOUT_SECS=\nEXTRA_FLAGS="--display-wake-timeout-secs=9"\n')
    assert via_flag["effective"]["display_wake_timeout_secs"] == 9
    validate('DISPLAY_WAKE_TIMEOUT_SECS=5\nEXTRA_FLAGS="--display-wake-timeout-secs=7"\n', valid=False)

    for layout in ["standard", "swap-left-win-alt", "swap-win-alt"]:
        for compatible in [0, 1]:
            report = validate(base + f"MODIFIER_LAYOUT={layout}\nMAP_CTRL_TO_CMD={compatible}\n")
            assert report["effective"]["modifier_layout"] == layout
            assert report["effective"]["map_ctrl_to_cmd"] is bool(compatible)
    old = validate(base + "MAP_CTRL_TO_CMD=1\n")
    assert old["effective"]["modifier_layout"] == "standard"
    assert old["effective"]["map_ctrl_to_cmd"] is True
    extra = validate('EXTRA_FLAGS="--modifier-layout swap-left-win-alt"\n')
    assert extra["effective"]["modifier_layout"] == "swap-left-win-alt"
    for invalid in ["MODIFIER_LAYOUT=swap\n", 'MODIFIER_LAYOUT=standard\nEXTRA_FLAGS="--modifier-layout standard"\n', 'MAP_CTRL_TO_CMD=0\nEXTRA_FLAGS="--map-ctrl-to-cmd"\n']:
        validate(invalid, valid=False)

    disabled = validate("ENABLE_H264=1\n")
    assert disabled["effective"]["capture_nv12"] is False
    for codec in [0, 1]:
        for nv12 in [0, 1]:
            text = f"ENABLE_H264={codec}\nCAPTURE_NV12={nv12}\n"
            report = validate(text)
            assert not report["warnings"], report["warnings"]
            assert report["effective"]["capture_nv12_requested"] is bool(nv12)
            assert report["effective"]["capture_nv12"] is bool(nv12 and codec)
    validate("CAPTURE_NV12=yes\n", valid=False)
    validate('CAPTURE_NV12=0\nEXTRA_FLAGS="--capture-nv12"\n', valid=False)
    mixed = validate(base + "ENABLE_H264=1\nCAPTURE_NV12=1\nAUTO_UNLOCK=1\nLOCK_ON_DISCONNECT=1\nMODIFIER_LAYOUT=swap-left-win-alt\nMAP_CTRL_TO_CMD=0\n")
    assert mixed["effective"]["capture_nv12"] is True
    assert mixed["effective"]["modifier_layout"] == "swap-left-win-alt"
    assert mixed["effective"]["map_ctrl_to_cmd"] is False
    assert mixed["effective"]["vd_width"] == 3840 and mixed["effective"]["vd_hidpi"] == "1"

    # The upstream TCP ack-lag limit is opt-in. Preserve absent/empty defaults,
    # the entire CLI u32 range, and the distinction between configured and active.
    for setting, requested in [("", 0), ("H264_MAX_UNACKED_FRAMES=\n", 0),
                               ("H264_MAX_UNACKED_FRAMES=0\n", 0),
                               ("H264_MAX_UNACKED_FRAMES=4\n", 4),
                               ("H264_MAX_UNACKED_FRAMES=4294967295\n", 4294967295)]:
        for codec in [0, 1]:
            report = validate(f"ENABLE_H264={codec}\n" + setting)
            assert not report["warnings"], report["warnings"]
            assert report["effective"]["h264_max_unacked_frames_requested"] == requested
            assert report["effective"]["h264_max_unacked_frames"] == (requested if codec else 0)
    for value in ["-1", "1.5", "4294967296", "false"]:
        invalid = validate(f"H264_MAX_UNACKED_FRAMES={value}\n", valid=False)
        assert any("H264_MAX_UNACKED_FRAMES" in error for error in invalid["errors"])
    for flag in ["--h264-max-unacked-frames 4", "--h264-max-unacked-frames=4"]:
        for prefix in ["", "H264_MAX_UNACKED_FRAMES=\n"]:
            report = validate(f"ENABLE_H264=1\n{prefix}EXTRA_FLAGS='{flag}'\n")
            assert not report["warnings"], report["warnings"]
            assert report["effective"]["h264_max_unacked_frames"] == 4
        for value in ["0", "4"]:
            invalid = validate(f"H264_MAX_UNACKED_FRAMES={value}\nEXTRA_FLAGS='{flag}'\n", valid=False)
            assert any("conflicts with EXTRA_FLAGS" in error for error in invalid["errors"])

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
