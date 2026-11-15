# macrdp — development fork

**English** | [简体中文](README_CN.md)

A native RDP server for macOS, built on Rust and IronRDP. This fork of
[clintcan/macrdp](https://github.com/clintcan/macrdp) develops reliable display
settings and connection lifecycle handling. It shares the logged-in Mac desktop;
it does not create an independent Windows-style login session.

## Branches

- `main`: an exact upstream `main` baseline. Synchronize upstream here without fork commits.
- `dev`: this fork's development, documentation, CI, and build artifacts. Merge upstream changes into this branch after synchronizing `main`.

The initial development base is upstream commit
[`29d01ed`](https://github.com/clintcan/macrdp/commit/29d01ed72946ebde25ca08c924429b5fc37de7c5).
The upstream README is retained as [README_UPSTREAM.md](README_UPSTREAM.md);
its release, verification, and support claims belong to upstream, not this fork.

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

Local checks: 394 Rust tests passed, 7 ignored; 10 Foundation configuration
checks and 14 configuration CLI process checks passed. Hardware behavior,
permissions, and actual client rendering still require manual validation.
Client-following and live resizing depend on the client sending the relevant
requests; fixed 4K mstsc checks do not validate every client or scale policy.
This fork makes no additional soak-test or production-readiness claim.

## Actions and maintenance

- `ci.yml`: Linux/macOS regression checks, configuration checks, and audit integration on `dev` changes and pull requests; also callable manually.
- `dev-build.yml`: manual Apple Silicon build with Rust/Swift checks, both app bundles, checksum and source-commit metadata. Artifacts are retained for 14 days; it does not publish a release.
- `release.yml`: inherited tag-based upstream release pipeline, extended to include the controller. Fork development builds use the workflow above.
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
