# Reliable display settings

The controller and server negotiate features with `macrdp --capabilities`.
Do not infer support from the package version: main-branch builds can share a
version number. The response identifies the source build and supported config
keys. An older server cannot safely validate the new settings.

## Resolution and scale

These are separate choices. Resolution is the RDP framebuffer in pixels; scale
is the size of the macOS interface within that framebuffer. For a native 4K
session with 200% UI scale, the expected result is a 3840 × 2160 RDP framebuffer
and a 1920 × 1080 macOS desktop in points.

```ini
VIRTUAL_DISPLAY=1
RESOLUTION_POLICY=fixed
VD_WIDTH=3840
VD_HEIGHT=2160
VD_HIDPI=1
LIVE_RESIZE=0
```

`PRIMARY_MODE` remains a separate choice about the physical Mac display. This
example does not change it. The virtual display belongs to the current macOS
desktop; it is not an independent login or a separate desktop per RDP client.

| Setting | Meaning |
| --- | --- |
| `RESOLUTION_POLICY=client` | Adopt the size requested by a newly connected client. Virtual-display width and height are startup defaults. |
| `RESOLUTION_POLICY=fixed` | Keep the configured size at connection and during the session. |
| `LIVE_RESIZE=1` | Allow later client layout requests when resolution follows the client. The client must actually send those requests. |
| `LIVE_RESIZE=0` | Keep the connection-time size during the session. |
| `VD_HIDPI=0` | Request 100% virtual-display UI scale. |
| `VD_HIDPI=1` | Request 200% virtual-display UI scale. |
| `VD_HIDPI=auto` | Experimental: map the announced client scale to the supported 100% or 200% mode. |

The legacy `EXTRA_FLAGS="--no-client-resolution"` selects fixed resolution if
no explicit resolution policy is present. Remove that flag when switching to
`RESOLUTION_POLICY=client`; contradictory settings are rejected. Preserve any
unrelated extra flags. `HIDPI` controls physical-display capture and does not
select virtual-display scale.

Automatic scale is not fractional scaling: client values below 200% select
100%, values at or above 200% select 200%. A missing initial scale selects 100%;
a missing scale in a later layout request keeps the existing scale. macOS's
private virtual-display API can land on a different mode, especially outside
its supported density range. Automatic 100% sessions wider than roughly 2048
pixels retain the existing upstream limitation. Select explicit 200% for the
4K example above and inspect the measured result.

Windows clients differ in their support for sending layout changes. Resizing
a client window or enabling client-side smart sizing does not prove that the
server received a new framebuffer request. Client-side stretching also does
not create a native HiDPI desktop.

## Validation and saving

```bash
macrdp --capabilities
macrdp --check-config /path/to/config.env
```

Both commands return JSON without starting a server, accessing credentials,
requesting macOS permissions, or creating a display. Validation exits nonzero
on invalid settings. It returns a config revision, warnings, errors, and a
redacted effective configuration. It does not prove that a display mode will
work on the current Mac, that a port is free, or that permissions are granted.

Config files are data, not shell scripts. `export` prefixes and matching outer
single or double quotes are accepted; substitutions and commands are never
executed. Conflicting duplicate keys are errors. Unknown keys are retained and
reported as warnings. The controller preserves unrelated keys and comments,
normalizes all occurrences of edited keys, and validates the entire candidate.

The controller checks for external edits, writes a private candidate, validates
it, and atomically replaces the config once. It keeps `config.env.previous`
as a recovery copy. Restart failures are reported and trigger rollback when
the live file has not been changed again. A concurrent edit must not be silently
overwritten by rollback.

## Requested versus measured state

`STATS_ENDPOINT=1` enables the existing read-only loopback endpoint (default
port 40245, configurable with `STATS_PORT`). It is optional. Schema version 2
adds process/config identity, startup state, and display state for both bitmap
and H.264 sessions. Display measurements that are unavailable are `null`.

The display object distinguishes the client request, negotiated wire dimensions,
and measured macOS points/backing pixels. It also includes session generation,
request sequence, phase, and a fallback reason. A request is not proof of the
actual mode. Old session state is cleared on disconnect; RDP reactivation keeps
the same session generation. Superseded delayed display work is cancelled before
its next operation starts; an OS operation already in progress cannot be undone.

`actual.scale_percent` is the effective remote UI scale (wire pixels divided
by macOS points). `actual.backing_scale_percent` is the Mac's rendering scale
(backing pixels divided by points). `actual.native_capture` confirms whether
both wire dimensions match the backing dimensions. A 200% remote UI ratio
alone is insufficient to claim native HiDPI: an upscaled low-resolution capture
can have the same ratio. The controller shows these measurements separately.
Initial client-requested dimensions remain unknown because Confirm Active can
echo the server's dimensions; only an actual MonitorLayout request establishes
the corresponding requested dimensions in this status API.

The controller verifies a restarted process against its PID and config revision
when the endpoint is enabled. Startup verification does not prove a remote
display is active: the actual mode is checked after connection. When telemetry
is disabled or unavailable, the UI reports that runtime display state is
unverified. A listening TCP port or a historical permission log is insufficient
evidence of the current display state.

## Regression checks

Automated tests cover parsing, redaction, conflicting settings, config revision,
transaction failure/rollback, fixed versus following policy, transient session
state, resize coalescing, helper timeout, and unknown measurements. The normal
suite does not create a real virtual display.

Run the process-level inspection smoke test after building the server:
`python3 scripts/check-config-cli.py target/release/macrdp`. It checks JSON/exit
codes, quoted flags, conflicts, redaction, missing files, and absence of runtime
file creation. It uses temporary fixtures and does not change a user config.

Before releasing a build, also exercise real client connections: fixed 4K at
200%; client-following 1080p and 4K; a scale-only change; rapid resize followed
by disconnect/reconnect; bitmap and H.264; and physical-screen restore for each
supported primary mode. Check pointer positions at all four corners and after
resizing. Exercise sleep/lock/wake separately: upstream display-identity and
blanking limitations are not solved by configuration validation.
