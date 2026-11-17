# macrdp — development fork

**English** | [简体中文](README_CN.md)

A native RDP server for macOS, built on Rust and IronRDP. This fork of
[clintcan/macrdp](https://github.com/clintcan/macrdp) develops reliable display
settings and connection lifecycle handling. It shares the logged-in Mac desktop;
it does not create an independent Windows-style login session.

## Branches

- `main`: an exact upstream `main` baseline. Synchronize upstream here without fork commits.
- `dev`: this fork's development, documentation, CI, and versioned releases. Merge upstream changes into this branch after synchronizing `main`.

The initial development base is upstream commit
[`29d01ed`](https://github.com/clintcan/macrdp/commit/29d01ed72946ebde25ca08c924429b5fc37de7c5).
The upstream README is retained as [README_UPSTREAM.md](README_UPSTREAM.md);
its release, verification, and support claims belong to upstream, not this fork.

## Included upstream updates

Upstream sync through [`fb1facf`](https://github.com/clintcan/macrdp/commit/fb1facf2d0666c8779a92b943221a4270432734d)
adds [#262](https://github.com/clintcan/macrdp/pull/262): optional TCP H.264 frame-ack
backpressure (`H264_MAX_UNACKED_FRAMES`, default `0`, disabled). It drops captures
when the client falls behind, retaining a 10 fps trickle; trailing flush frames
are exempt on TCP so the final change can appear. This is a pacing threshold,
not a strict maximum queue size. Upstream reports Windows App and mstsc testing;
this fork has not yet measured the enabled setting with its 4K/200% client.
Configuration inspection reports both requested and effective values; the option
has no effect without H.264. The sync also includes #268's late-S_OK lossy-audio
regression test and records #230's already-integrated startup fix in the merge
history. Existing fork lock/display/clipboard policies remain in place.

## Current release

**Latest: `v0.10.8`.** Subsequent builds keep this application version and
release tag. The package's `SOURCE_COMMIT` identifies the build.
See the [changelog](CHANGELOG.md#v0108) for the security and clipboard fixes.

If a locked Mac's displays go to sleep and ScreenCaptureKit loses its source,
the server ends that RDP session and runs the normal display cleanup. Other
explicit capture stops get up to three seconds to recover a usable frame.
An ordinary lock, idle desktop or minimized client keeps a healthy session open.

Recent releases:

- `v0.10.6-dev` included upstream Windows 10 mstsc display fixes #228 and #230.
  The update still needs a Windows 10 client retest.

## This same-version rebuild

Review fixes in this rebuild keep file descriptors and source paths in one
announcement snapshot, reject malformed descriptor lists without reindexing,
and discard retired-session file requests/lease releases. Disconnect lock
confirmation checks whether authentication is actually required, including the
screen-saver grace deadline. Remote-only audio open/read failures share a finite
retry budget. Settings restart allows native shutdown to finish before checking
the new process; newly installed LaunchAgents allow 60 seconds for cleanup.
See the [review and verification record](docs/release-review-2026-11-17.md) for
source references, completed tests and remaining manual checks.

The subsequent dependency-security source update selects `libz-sys 1.1.33`
and official stable `rsa 0.10.0`, removing the old RSA advisory exception.
Three narrow [authentication adapters](vendor/AUTH-SECURITY.md) preserve the
current protocol stack while replacing exact prerelease pins. Stable RSA rejects public keys below 1024 bits.
Dependency regression tests also caught an upstream EC coordinate-padding bug;
its fix and upstream JWE error masking are backported. Clipboard fallback staging
now uses atomic private directories to avoid concurrent timestamp collisions.

On 2026-11-18, the user confirmed Directory Opus file copying and normal screen
locking/unlocking with the installed build from `3bea2b7`. This covers those tested
paths; monitor-free operation, rapid reconnect/resize and relocking after abandoned
pre-capture preparation remain separate verification boundaries.

Earlier feedback on build `31ee2fb` reported working remote audio and Windows
desktop file copies, but failing Directory Opus copies. The follow-up gives a failed
`FileGroupDescriptorW` request one retry and reports a second failure explicitly,
without substituting text or old images. The later `3bea2b7` test confirms the user's
Opus copy path works; clients must still supply a valid standard file stream.

Detach restoration now retains every display the server disabled, even if it
disappears from CoreGraphics' online list. A successful transaction alone no
longer counts as recovery: every saved display must be confirmed active. Failed
restoration uses bounded recovery; with disconnect locking enabled, a recovery
restart requires a confirmed lock and must not interrupt a newer session.
The user verified automatic two-monitor recovery on `f2d30dc`. Logs showed about
2.5s of disconnect grace, 10.5s of native display restoration, then a 0.13–0.15s
lock; the desktop was visible before that lock. The correction locks Detach
sessions before restoration and holds the auto-unlock gate through the native
transition. Capture/Shield retain their existing restore-first ordering. The
completed transition must not schedule a second, delayed lock. A live probe
confirmed locking with only the virtual display active in 102ms; full timing
measurements and edge-case coverage are separate from the user's 2026-11-18 confirmation
that normal locking/unlocking works on `3bea2b7`.
In the reported failure, restarting the old server did not restore either output;
reconnecting a display cable restored both. Process restart is a recovery attempt,
not a guarantee that a stuck display link will recover.

New remote clipboard announcements now invalidate old text/images even when no
supported format is offered. File URLs are explicitly serialized and read back
before publication is reported as successful; File Provider downloads remain
on demand. Unsupported Windows private formats still need a standard
`FileGroupDescriptorW` stream from the client.

Permissions adds an optional Full Disk Access shortcut for Mac-to-Windows copies
from protected locations. Grant it to the server `macrdp.app` in System Settings;
this does not repair incoming Windows formats. Disconnect locking removes the
old extra 22.5-second default wait, but still waits for the 2.5-second reactivation
grace and any bounded reconnect hold. Detach locks before restoring physical
displays; the other modes restore first. It is not
an instantaneous lock or a local-input takeover detector. At 4K/200%, macOS's
1920×1080 logical workspace is distinct from the 3840×2160 RDP output.

File Provider files now expose owner-writable local copies rather than locked
read-only items. Local edits are persisted with version checks and are never
uploaded to the Windows source. Ten adapter checks and eight synthetic system
checks cover on-demand reads, editable exported copies, offline cache eviction/
rehydration and reconnect isolation. Those automated checks alone do not establish
real mstsc behavior; the `3bea2b7` Opus copy result above is a separate client test.
The menu-bar status tooltip is removed; status remains in the menu and Status tab.
Use `SOURCE_COMMIT` to identify this rebuild; remaining client checks are recorded
in the [review record](docs/release-review-2026-11-17.md).

The default-off experimental **Audio → Remote-only playback** option
(`AUDIO_REMOTE_ONLY=1`) uses a private Core Audio tap on macOS 14.2+. It suppresses
local playback only while capturing, without changing system volume, mute or the
default output device. System audio recording permission is required. Initialization
failure is reported; it does not silently fall back to dual playback. An established
tap shares up to five bounded retries across open and read failures; only valid
stereo samples reset that budget; initial permission failures require correction. The existing
minimize policy still pauses forwarded audio; disconnect destroys the tap to restore
local playback. Status reports capture state/errors, not confirmed client audibility.
An isolated local probe captured a nonzero 48 kHz test tone and destroyed the tap
successfully. The user confirmed mstsc playback on `31ee2fb`; output-device
changes, reconnects and abnormal exit still need live validation. The direct speaker-mute
control test did not preserve the remote tone, so that approach is not used.
The earlier intermittent Mac-only playback recovered after restart; its separate
root cause is not yet confirmed. Disconnect locking additionally checks capture
session epochs, handles coalesced transitions and serializes with auto-unlock.

Monitor-free, already logged-in sessions use the same disconnect-lock policy.
Detach/Blank do no physical transactions when no other display is online. The
virtual surface is retained through lock completion/cancellation and can then be
released; online-but-inactive displays remain a restoration concern. Authenticated
headless reconnect prepares a temporary virtual surface before auto-unlock and
retains its lease until in-flight password work ends, even if its waiter cancels.
Shield still requires helper acknowledgement with zero targets, retaining hot-plug
protection. Automated regression coverage is added; actual unplug/replug and
monitor-free locked reconnect testing with the rebuilt server remain pending.
This covers logged-in user sessions, not FileVault/pre-login boot screens. An
attempt cancelled during password input can still finish unlocking. Automatic
re-lock is not guaranteed if preparation is abandoned or capture fails after
unlocking, before it enters the counted session lifecycle.

## Changes on dev

| Area | Implemented behavior |
| --- | --- |
| Display policy | Explicit client-following or fixed resolution; live resize is a separate setting. Automatic UI scale follows actual client announcements. |
| Configuration | Shared parser and side-effect-free `--capabilities` / `--check-config`; staged validation, atomic saving, activation verification, and recovery in the controller. |
| Actual status | PID and configuration revision, startup permissions, requested/negotiated pixels, measured backing pixels and logical points, native capture and fallback state. Unknown measurements remain unknown. |
| Session lifecycle | Latest-layout mailbox, session generations, stale-work rejection, and input mapping based on measured dimensions. |
| Reconnect stability | QoE-based black-screen recovery defaults off: zero-millisecond timing cannot prove a blank screen. Physical-display install/restore runs serially outside async network workers. |

The Display panel exposes these policies and separates requested settings from
measured state. See [display settings](docs/display-settings.md),
[controller guide](gui/README.md), and [CLI reference](docs/cli.md).

## Virtual display lifetime

The virtual display is created for authenticated capture, not at server startup.
After the last capture ends, a 2.5-second reactivation grace and physical-screen
restoration precede removal. Quick reconnects cancel stale cleanup. Fresh display
IDs are used by capture, input, audio and the switcher HUD. Newly created displays
and confirmed unlocks wait up to 8 seconds for SCK registration, returning as soon
as the requested display appears; this is a fixed maximum, not an 8-second delay.

If an online physical display fails to become active, retain the virtual display.
When no other display is online, it may be released after the disconnect lock
completes; a locked headless reconnect prepares a new virtual surface before unlock. Configuration (including
fixed 4K/200%) remains saved for the next connection. Normal locking/unlocking was
confirmed on `3bea2b7` on 2026-11-18; monitor-free reconnects, hot-plug and rapid
resolution changes still need hardware/client coverage. Publishing alone does not
replace an installed app or local configuration.

## Modifier positions and Ctrl shortcut compatibility

These are independent settings in **Input → Modifier key positions** and
**Windows editing compatibility**. Missing `MODIFIER_LAYOUT` means `standard`;
existing `MAP_CTRL_TO_CMD` settings keep their meaning.

| Physical Windows key | `standard` | `swap-left-win-alt` | `swap-win-alt` |
| --- | --- | --- | --- |
| Left Ctrl | Control | Control | Control |
| Left Windows | Command | Option | Option |
| Left Alt | Option | Command | Command |
| Right Windows | Command | Command | Option |
| Right Alt | Option | Option | Command |

To put Command next to the space bar while preserving Ctrl:

```dotenv
MODIFIER_LAYOUT=swap-left-win-alt
MAP_CTRL_TO_CMD=0
```

Left Alt+C/V then sends Command+C/V, and left Alt+Tab sends Command+Tab.
Ctrl+C stays Control+C, including in terminals. Independently enable
`MAP_CTRL_TO_CMD=1` to keep selected Windows editing shortcuts and Ctrl+click
as Command equivalents, with the existing terminal/application exclusions.
This compatibility mode does not turn every Ctrl combination into Command.

`swap-win-alt` also swaps the right keys and may interfere with AltGr text input;
the left-only mode leaves right Alt unchanged. Mapping applies to received RDP
scancodes, including matching releases and keyboard/mouse modifier flags.
It does not remap the local Mac keyboard. Clients may intercept combinations
before sending them; server mapping cannot recover events it never receives.
Option-based optional app shortcuts follow the mapped Option key (Windows after
swapping); the UI shows the physical key hints. Maintenance shortcuts keep
physical **Ctrl+Alt+G** and **Ctrl+Alt+Shift+R** in every mode.

CLI: `--modifier-layout standard|swap-left-win-alt|swap-win-alt` and the separate
`--map-ctrl-to-cmd`. Applying changes restarts the server. Unknown layout values
and conflicting dedicated/extra-flag mappings fail validation. Automated checks
cover the mapping and state transitions; this new feature has not yet been
validated with real Windows shortcut/AltGr input.

Upstream `v0.10.6` (`0255a8b`, #267) is merged. If a launchd-started server has
missing/wrong stored credentials, it now waits and rechecks only a changed
password instead of entering a failed-login restart loop. The controller shows
the blocked state and offers password correction. This remains separate from
per-connection auto-unlock, and validation commands remain side-effect free.

## Upstream NV12 capture experiment

Merged upstream [cef6073](https://github.com/clintcan/macrdp/commit/cef6073ba461c595261a2705a2c84888cd522673)
([#204](https://github.com/clintcan/macrdp/pull/204)): `--capture-nv12` /
`CAPTURE_NV12=1` lets ScreenCaptureKit produce full-range BT.709 NV12 for direct
VideoToolbox encoding. It requires H.264 to have an effect and remains **off by
default**. Non-AVC/initial bitmap frames still use a BGRA fallback. The upstream
CPU improvement measurements are upstream results, not measurements on this Mac.

The merge preserves authenticated pre-capture unlocking, fixed 4K/200% display
policy, keyboard mapping and File Provider clipboard handling. Config inspection
reports requested versus effective NV12, validates its boolean and rejects a
dedicated-key/Extra-flags duplicate. No local configuration is enabled or changed.
Actual Windows/NV12 rendering after this merge remains pending manual validation.

## Locked connections and disconnect locking

With a headless display mode and `AUTO_UNLOCK=1`, the fork prepares the desktop
**after RDP credential/ARC validation and before ScreenCaptureKit capture**.
This removes the circular dependency where a locked/display-asleep Mac reported
`SCK sees: []` and disconnected before the session watcher could unlock it.
After a confirmed unlock, capture waits at most 8 seconds for its requested
display. Unsafe/unknown lock state and exhausted password-submission budget stop
capture with a diagnostic; the two-submission limit remains shared and serialized.
`AUTO_UNLOCK` remains opt-in and is disabled with `--skip-auth`.

`LOCK_ON_DISCONNECT=1` now requests an immediate password-required session lock
via runtime-resolved private login.framework and confirms the system lock flag
for up to 2 seconds. It does not depend on the screen-saver password delay,
change security preferences, or silently fall back to a screen saver. Missing API,
request errors and unconfirmed lock state are reported as failures. The existing
physical-display restoration, disconnect grace and bounded
reconnect hold remain; the extra safety delay defaults to zero; **disconnect locking is delayed, not instantaneous**.
Auto-unlock affects the physical desktop, and these private macOS APIs may change.

Regression tests cover preparation ordering/failure, serialization, display
selection, lock confirmation and existing reconnect/budget policy. **The user
confirmed normal locking/unlocking on installed source `3bea2b7` on 2026-11-18.**
This does not verify monitor-free reconnects or guarantee relocking when preparation
is cancelled or capture fails before the session is counted.
Build verification does not grant TCC permissions or change the running app.

## Windows clipboard images and files

PNG/DIB screenshots take priority over their virtual image-file representation.
Explorer file copies use the file stream even when a thumbnail is offered.
While files are being prepared, an inert clipboard item replaces stale text;
file names and previews never fall back to plain-text clippings. A newer local
copy wins over delayed remote replies. The complete-file fallback plays its
ready sound without generating Cmd+V: paste again when the files are ready.

Mac→Windows clipboard reads/encoding run on bounded blocking workers, allowing
RDP to service input and File Provider hydration while a read waits. Limits are
4 workers, 32 queued/completed operations and 30 seconds per request including
queue time; slow or stalled reads fail rather than blocking the connection.
Uncancellable OS reads retain their worker slot until they finish. Format replies
remain ordered; file requests retain their original locked path snapshot and
late completions from disconnected sessions cannot enter a new connection.
Completed replies retain their budget through dispatch and keep clipboard priority
ahead of video in each batch; batches are capped at 32 events so inbound protocol
work gets another lock turn. This is not a socket-write deadline: TCP congestion
can still delay a large reply. Automated checks do not establish that the reported
mstsc freeze is resolved; the new build still needs a real-client copy test.
See the [clipboard investigation and upstream review](docs/clipboard-followup.md).

Upstream [#230](https://github.com/clintcan/macrdp/pull/230) is included: during
initial H.264 negotiation, hold the first legacy picture for up to 3 seconds and
submit it when EGFX becomes ready, including a static desktop. If EGFX does not
start, fall back to legacy rendering. `MACRDP_EGFX_LEGACY_HOLD_MS=0` disables this
hold. Upstream Windows 10 results are not a local verification of this fork.

Bundled macOS 15+ builds now include a sandboxed **macrdp Files** helper and
File Provider extension. Enable it in **System Settings → General → Login Items
& Extensions → File Providers**, then restart macrdp. Flat file copies of any
type can publish genuine system dataless URLs: directly reading/copying the file
triggers its download. Each copy has its own immutable session/copy/lock identity
and directory, so same-name reconnects cannot silently substitute new content.
No App Group or Developer ID certificate is required for the tested local path;
ad-hoc signing is still not notarized distribution validation.

The source broker listens only on loopback, uses a per-process random token
passed in domain configuration, and requests serial 128 KiB ranges with the
original lock. The extension streams into private files and returns a URL only
when complete. Incremental metadata anchors survive extension restarts. New
copies lose the previous copy's right to publish without cancelling its existing
readers. Unsupported/unavailable provider paths, older macOS, `--no-lazy-paste`,
and folders use a **complete-file** fallback; sparse presenter placeholders are
no longer published. File-only image fallback also provides decoded PNG data
within its existing image safety limits.

Bounds: 4,096 items per batch/catalog, 128 GiB per file; the provider broker keeps
at most 64 copies, retires non-current copies after 10 minutes idle, and source
locks expire after at most two hours. Recopy an expired source. File transfers
require negotiated clipboard locking. These bounds may also reject overly large
or ambiguous copies; a failed transfer never publishes a zero-filled substitute.
Downloaded OS files remain readable offline; unmaterialized disconnected files
fail rather than reading a new session's bytes. The domain is removed at server
shutdown; definitely dead owners are reaped at the next startup.

Automated checks include the Rust suite, nine adapter/IPC checks (including a
>16 MiB streamed file), and six real File Provider checks with synthetic bytes:
zero-range dataless metadata, direct hydration, offline reads, disconnected reads,
same-domain/same-name reconnect, and stale URL rejection. The user reported successful
testing of the previously installed `92f4c8c` build. On 2026-11-18, the user also
confirmed Directory Opus file copying on installed source `3bea2b7`; this does not
cover every file type, folder size or disconnect-during-transfer case. See
[implementation and checks](native/file-provider/README.md) and
[changelog](CHANGELOG.md#v0106-dev).

## Build and run

The current artifact target is **Apple Silicon macOS**. Rust 1.95 and a Swift 5.9+
compatible macOS SDK are used in CI; local development also used Rust 1.100.
Install the Xcode command-line tools, Rust, and CMake before building.

```sh
git clone --branch dev https://github.com/zhangyxXyz/macrdp.git
cd macrdp
cargo build --release --locked
# Assemble both apps into a staging directory (choose a writable absolute path).
APP_DIR="$PWD/target/apps" packaging/make-app.sh
APP_DIR="$PWD/target/apps" gui/make-tray-app.sh
```

Install both apps together and follow the [packaging guide](packaging/README.md)
to configure the LaunchAgent and credentials. Enable **Screen Recording** and
**Accessibility** for the installed macrdp app, then restart the server.
The default bundle IDs remain compatible with upstream's controller/LaunchAgent;
these apps replace an upstream installation rather than running alongside it.
Back up existing apps and configuration before replacing them.

Fork artifacts use **ad-hoc signing**, without Developer ID notarization.
Rebuilding changes the signing hash and may require removing/re-adding the app
in macOS privacy settings. Upstream Homebrew commands install upstream releases,
not this fork. No camera system extension is included by the fork build workflow.

## Native 4K with 200% UI

Merge these keys into `~/Library/Application Support/macrdp/config.env`;
preserve your own bind address, credentials, TLS and device settings.

```dotenv
VIRTUAL_DISPLAY=1
PRIMARY_MODE=detach
VD_WIDTH=3840
VD_HEIGHT=2160
VD_HIDPI=1
RESOLUTION_POLICY=fixed
LIVE_RESIZE=0
ENABLE_H264=1
STATS_ENDPOINT=1
BLANK_RECOVERY=0
```

Validate before applying:

```sh
/Applications/macrdp.app/Contents/MacOS/macrdp --check-config "$HOME/Library/Application Support/macrdp/config.env"
```

Native 4K/200% means 3840×2160 negotiated and backing pixels with 1920×1080
logical points. Client-side bitmap stretching is a separate setting. Applying
server settings restarts the server and disconnects the current session.

## Verification and limits

On 2026-11-15, one local Apple Silicon Mac and Windows mstsc build 26100 were
used to check 3840×2160 native capture, 200% UI, and a 60 Hz virtual display.
The fixed-4K configuration stayed connected for 6 minutes 12 seconds, followed
by two reconnects without automatic QoE recovery drops. Server-side H.264
readiness after accept was about 0.73–0.75 seconds on reconnect; this does not
measure client-visible first-frame latency. Physical-screen transitions still
took about 10.5 seconds. A 60 Hz mode / 60 fps target is not a sustained-fps guarantee.

During the earlier `v0.10.6-dev` rebuild: 491 Rust tests passed, 8 ignored; strict Clippy,
10 Foundation configuration checks and 33 configuration CLI process checks passed.
The native audio-buffer checks passed with AddressSanitizer/UndefinedBehaviorSanitizer. Hardware behavior,
permissions, and actual client rendering still require manual validation.
Client-following and live resizing depend on the client sending the relevant
requests; fixed 4K mstsc checks do not validate every client or scale policy.
This fork makes no additional soak-test or production-readiness claim.

## Actions and maintenance

- `ci.yml`: the reusable regression gate for Linux/macOS tests, strict Clippy, configuration, native adapters, release scripts and audit integration. All macOS checks share one runner; Linux checks run alongside them. It also runs on branch changes/PRs and can be started manually.
- `release.yml`: only `v*` tag pushes trigger `build/package → draft upload/download verification → publish → cleanup`. CI remains independent and is not a blocking release dependency; run the required review/regressions before tagging. The tagged commit must belong to `dev` and its tag must match the application version, currently `v0.10.8`. Both apps, source metadata and checksum go directly to a draft Release; there are no intermediate Actions Artifacts.
- `build-check.yml`: manual-only `checks → build/package verification`, without publishing or uploading artifacts. Ordinary dev pushes do not run a packaging job.
- Same-tag rebuilds use a separate `staging-<tag>-<source>` draft and stable Release/asset IDs. The current download remains available until the replacement passes round-trip verification and ownership checks. Publication replaces only the owned Release, preserving that version tag. GitHub has no atomic Release swap: a final publish failure leaves the verified draft for retry. Tag/source checks reject obsolete runs. Re-run a failed workflow to resume its owned draft.
- Native jobs default to Apple Silicon `macos-26`. `MACOS_RUNNER` can select another supported macOS runner; `macos-26-intel` is a capacity fallback. Intel runs host regressions plus ARM64 Clippy/compilation; packaging explicitly targets ARM64 for every component and verifies architecture, source identity and nested signatures. Intel cannot execute the packaged ARM64 binary: `BUILD_INFO.json` records the host and the same-source host executable that produced static capabilities. Actual ARM64 execution remains a separate check. Workflow names consistently use lowercase.
- Release notes use the [short bilingual template](packaging/RELEASE_NOTES.md) and link to that version in [CHANGELOG.md](CHANGELOG.md). Add the changelog section before tagging; the full section is not duplicated in release notes.
- `cleanup-releases.yml`: manual preview defaults on. The release pipeline also prunes after publishing: verify the newest owned release first, retain it, remove older pipeline-owned releases and their matching tags. Unknown releases and drafts are untouched; failed verification or concurrent edits abort cleanup. Inspired by CLIProxyAPI-Plugins. The former manual `dev-build.yml` artifact build has been removed.
- `security.yml`: dependency checks, including `dev` dependency changes.
- `delete-old-workflow-runs.yml`: adapted from [CLIProxyAPI-Plugins](https://github.com/zhangyxXyz/CLIProxyAPI-Plugins). Keeps each workflow's six newest runs and all runs younger than 30 days; only completed older runs are eligible. Push runs are previews; manual runs default to preview; the weekly schedule deletes eligible runs.

Scheduled workflows run from the repository's default branch (`dev`). To update
upstream, fast-forward `main` to `upstream/main`, then merge `main` into `dev`.
Resolve fork conflicts on `dev`; keep `main` free of README and workflow edits.

## License and thanks

Fork modifications retain the upstream **MIT OR Apache-2.0** licensing; see
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

Thanks to **[clintcan](https://github.com/clintcan)** and the
[macrdp contributors](https://github.com/clintcan/macrdp/graphs/contributors)
for the original server, protocol integration, packaging, and documentation.
Thanks also to [IronRDP](https://github.com/Devolutions/IronRDP) and its contributors.
