# macrdp Controller (menu-bar app)

A small AppKit menu-bar (tray) app that **controls** the macrdp LaunchAgent and
its `config.env` — start/stop the server, flip feature toggles, jump to the
permission panes, and open the log. It's a separate controller process, not the
server: quitting it leaves macrdp running under launchd.

## Why a separate process (not UI inside the Rust binary)

macrdp's tokio runtime owns the main thread, but AppKit's menu bar **must** run
on the main thread — so an in-process UI would mean restructuring the carefully
tuned threading/QoS model. The controller sidesteps that entirely: it drives
the server through `launchctl` and the shared `config.env`. TCC is unaffected —
the Screen Recording / Accessibility grants belong to the macrdp *binary* (the
API caller), and this controller needs none of them.

## Build & install

Prereq: build the server bundle (`../packaging/make-app.sh`). You do **not**
need to run `install-launchagent.sh` — the controller **self-installs** the
LaunchAgent and onboards the Keychain password on first **Start** (see below).

```bash
./make-tray-app.sh                                  # -> /Applications/macrdpController.app
APP_DIR="$HOME/Applications" ./make-tray-app.sh     # or install without sudo
open "/Applications/macrdpController.app"            # display icon appears in the menu bar
```

Built with plain SwiftPM (no `.xcodeproj`): `swift build -c release` produces
the executable, `make-tray-app.sh` wraps it into a signed `LSUIElement` bundle
in `target/` and installs it.

The bundle-ID prefix is configurable with `BUNDLE_PREFIX` (default `com.clintcan`)
— the controller becomes `$BUNDLE_PREFIX.macrdp.controller` and derives the
server's LaunchAgent label by stripping `.controller` at runtime. **Use the same
`BUNDLE_PREFIX` here as in `../packaging/`**, or the controller drives the wrong
agent.

## First-run self-install

The intended end-user flow is just: drag both apps from the DMG into
`/Applications`, open the controller, click **Start**. On first Start the
controller:

1. **Locates `macrdp.app`** (sibling in the same folder, else `/Applications`,
   else `~/Applications`).
2. **Prompts for your macOS account password** and stores it in the Keychain
   (the headless server reads it from there; written via `security` so there's
   no read-time Keychain prompt).
3. **Writes + loads the LaunchAgent** pointing at that server bundle.
4. **Reminds you to grant** Screen Recording + Accessibility, with a button to
   open the pane.

No Terminal, no `install-launchagent.sh`. For unattended/MDM deploys there's a
headless equivalent:

```bash
macrdpController.app/Contents/MacOS/macrdptray --install-agent   # locate + write + load agent
macrdpController.app/Contents/MacOS/macrdptray --print-paths      # diagnose resolved paths (no side effects)
```
(`--install-agent` assumes the Keychain password is set separately.)

## Settings and display behavior

Open **Show macrdp… → Display**. Changes stay in a draft until **Save** (server
stopped) or **Apply & restart**. Applying while running disconnects remote
sessions. **Reload** discards the draft and reads external edits.
Double-clicking the running controller app also opens Settings, or brings its
existing window forward without interrupting an Apply operation.

- **Resolution policy:** follow the RDP client's requested size, or keep a fixed
  Mac display pixel size. This follows the RDP request, which is not necessarily
  the remote monitor's native resolution.
- **Initial / fallback pixel size:** common presets include 3840 × 2160 (4K).
  Existing custom sizes are retained; width and height can also be entered.
- **Allow resolution changes during a session:** controls dynamic resolution
  requests when following the client. A compatible client must send those
  requests. It does not disable independent automatic scale selection.
- **Mac interface scaling:** 100%, 200% HiDPI, or follow client scale
  (experimental). 3840 × 2160 with 200% requests a 1920 × 1080 point workspace
  rendered at 4K. Auto selects supported Mac scaling; arbitrary Windows scale
  factors are not promised.
- **Primary screen:** keep on, detach, or blank. The existing takeover and lock
  controls remain available. Physical-display HiDPI capture is shown in Video
  only when virtual display is off.

The controller probes `--capabilities` on the LaunchAgent's server executable.
New display controls require advertised key support, and saving requires
`--check-config`. A matching version label alone is insufficient. Unsupported
servers show an explanation instead of silently accepting unsupported options.

**Status** reports client-requested pixels and scale, RDP output pixels, Mac
workspace points, backing pixels, remote interface scale, Mac rendering scale and
native pixel capture separately. Unknown or missing
fields stay **Unknown**. Display phase is independent of server startup and
H.264. Fallback details are shown only when the server reports a degraded display.
The current session state comes from the server; TCP observations are labeled
separately and do not prove a negotiated client capability. Historical log
fingerprints are not presented as current client identity. The Permissions pane
shows the current server’s startup checks, explicitly not live TCC status.

Enable **Advanced → Live statistics endpoint** to obtain schema 2 display and
startup status over loopback. The controller does not enable it automatically.
With it off, Apply reports only a saved file and a stable restarted process;
it explicitly leaves runtime display status unverified. When enabled, startup
verification requires the new process ID, matching configuration revision and
startup phase. No RDP login or display session is required to apply settings.

## Reliable configuration saving

The controller reads the server's plain `KEY=value` grammar, including `export`
followed by whitespace and one matching pair of single or double quotes. It
never executes shell syntax. Last duplicate values are read consistently; when
a key is edited, its occurrences become one canonical assignment. Unrelated
keys, comments and `EXTRA_FLAGS` are preserved. Validation conflicts must be
resolved explicitly; validation warnings are presented for review before saving.

A private candidate is validated without side effects, then atomically replaces
the whole file once. External edits detected since loading or during validation
abort the save. Sensitive temporary and recovery files are created with owner-only
permissions from the first byte (preserving stricter existing owner permissions).
The previous bytes are kept in `config.env.previous`. A failed restart or startup
verification restores those exact bytes and attempts to restart the previous
configuration; the UI reports both failures if recovery cannot be verified.
An external edit after replacement is never overwritten by rollback.

## Build verification without installing

From the repository root:

```bash
swift build --package-path gui --scratch-path gui/.build --product macrdptray
swift run --package-path gui --scratch-path gui/.build ConfigSupportTests
```

The Foundation-only test executable covers parsing, duplicate keys, comment and
unknown-key preservation, rejected candidates, external edits, atomic save,
rollback, concurrent controllers, private file permissions, EXTRA_FLAGS grouping
and protocol version/unknown-value handling.
It does not launch the server or touch user configuration. This target works with
Command Line Tools installations that do not include XCTest.

On CLT installations whose macOS 27 SDK requires an unavailable SwiftUIMacros
plugin, an already installed macOS 26.5 SDK can be selected for this build only:

```bash
swift build --package-path gui --scratch-path gui/.build -c release \
  --sdk /Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk --product macrdptray
swift run --package-path gui --scratch-path gui/.build \
  --sdk /Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk ConfigSupportTests
```

When installing later, `gui/make-tray-app.sh` accepts the same SDK path through
`SWIFT_SDK=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk`. The install
script builds only the controller product.

The build-only commands above do not change `xcode-select`, install Xcode, install the controller, or
restart a service. Full interactive testing still requires launching a staged
controller against a disposable test service; compilation alone does not verify
macOS virtual display or real Windows client negotiation.

## Distribution (paid product)

Ad-hoc signing is local-only. For a shippable build, sign with a Developer ID
and notarize (same env contract as `packaging/`):

```bash
xcrun notarytool store-credentials macrdp-notary \
  --apple-id you@example.com --team-id TEAMID --password <app-specific-pw>

CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
  NOTARIZE=1 NOTARY_PROFILE=macrdp-notary ./make-tray-app.sh
```

This is MIT-licensed; selling a productized, notarized build + support is fully
compatible with that (you're selling the product, not a license exemption).
The Mac App Store is not viable for the server it controls (private
`CGVirtualDisplay` API + system-wide input/capture) — ship a direct download.

## Notes
- To auto-launch the controller at login: System Settings → General →
  Login Items → add `macrdpController.app` (the server itself already
  autostarts via its own LaunchAgent).

## Remote modifier positions

Input now separates modifier positions (`MODIFIER_LAYOUT`) from the existing
Ctrl editing compatibility (`MAP_CTRL_TO_CMD`). Standard, left Win/Alt swap,
and both-side swap are available when the server advertises `modifier_layout`.
The left-only swap leaves right Alt unchanged; both-side swap may affect AltGr.
Option shortcut labels display the physical keys for the selected policy.
Physical maintenance chords remain Ctrl+Alt+G and Ctrl+Alt+Shift+R. Settings are
staged until Apply and restart, with server validation and activation checks.
