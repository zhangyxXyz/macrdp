import AppKit
import SwiftUI
import Combine
import ConfigSupport

// The tabbed Settings window opened by "Show macrdp…". A SwiftUI TabView hosted
// in a plain NSWindow (the status-bar item stays AppKit). While the window is
// open the app switches to a .regular activation policy so it behaves like a
// real app (Dock icon + reliable focus + a working Edit menu for the text
// fields); it drops back to .accessory (menu-bar-only) when the window closes.

final class SettingsWindowController: NSWindowController, NSWindowDelegate {
    let model: SettingsModel
    private let onClose: () -> Void

    init(controller: AppController, onClose: @escaping () -> Void) {
        self.model = SettingsModel(controller: controller)
        self.onClose = onClose
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 580),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered, defer: false)
        window.title = "macrdp Settings"
        window.contentViewController = NSHostingController(rootView: SettingsView(model: model))
        window.isReleasedWhenClosed = false
        window.center()
        super.init(window: window)
        window.delegate = self
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not supported") }

    func show() {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(nil)
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard !model.isApplying else { return false }
        guard model.isDirty else { return true }
        let a = NSAlert()
        a.messageText = "Discard unsaved changes?"
        a.informativeText = "Some settings haven't been applied yet. Close and discard them?"
        a.addButton(withTitle: "Discard")
        a.addButton(withTitle: "Cancel")
        return a.runModal() == .alertFirstButtonReturn
    }

    func windowWillClose(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        onClose()
    }
}

// MARK: - Controller wiring

extension AppController {
    /// Open (or bring forward) the Settings window.
    @objc func showSettings() {
        if settingsWindowController == nil {
            settingsWindowController = SettingsWindowController(controller: self) { [weak self] in
                self?.settingsWindowController = nil
            }
        }
        settingsWindowController?.show()
    }

    /// Open the Settings window (if needed) and navigate it to a section — the
    /// action behind the main-menu "Section" items.
    @objc func selectSection(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String,
              let s = SettingsSection(rawValue: raw) else { return }
        showSettings()
        settingsWindowController?.model.section = s
    }

    /// A minimal main menu so the standard editing shortcuts (Cmd+C/V/X/Z/A) work
    /// in the window's text fields and Cmd+W/Cmd+Q behave. Set once at launch; it
    /// only shows while the app is .regular (the Settings window open).
    func installMainMenu() {
        let main = NSMenu()

        let appItem = NSMenuItem()
        main.addItem(appItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "About macrdp", action: #selector(showAbout), keyEquivalent: "")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Hide macrdp", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Quit macrdp Controller", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu

        let editItem = NSMenuItem()
        main.addItem(editItem)
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        editMenu.addItem(withTitle: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = editMenu

        // Section menu — the same categories as the sidebar, navigable from the
        // menu bar (⌘1…⌘8). Each item drives SettingsModel.section.
        let sectionItem = NSMenuItem()
        main.addItem(sectionItem)
        let sectionMenu = NSMenu(title: "Section")
        for (i, s) in SettingsSection.allCases.enumerated() {
            let mi = NSMenuItem(
                title: s.title, action: #selector(selectSection(_:)),
                keyEquivalent: i < 9 ? String(i + 1) : "")
            mi.target = self
            mi.representedObject = s.rawValue
            sectionMenu.addItem(mi)
        }
        sectionItem.submenu = sectionMenu

        let windowItem = NSMenuItem()
        main.addItem(windowItem)
        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(withTitle: "Close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        windowMenu.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        windowItem.submenu = windowMenu

        NSApp.mainMenu = main
    }
}

// MARK: - Sections

/// The settings categories — shown all-at-once in the sidebar AND as a "Section"
/// menu in the main menu bar (each drives `SettingsModel.section`).
enum SettingsSection: String, CaseIterable, Identifiable {
    case status, connection, video, audio, display, input, redirection, advanced, permissions
    var id: String { rawValue }
    var title: String {
        switch self {
        case .status: return "Status"
        case .connection: return "Connection"
        case .video: return "Video"
        case .audio: return "Audio"
        case .display: return "Display"
        case .input: return "Input"
        case .redirection: return "Redirection"
        case .advanced: return "Advanced"
        case .permissions: return "Permissions"
        }
    }
    var icon: String {
        switch self {
        case .status: return "gauge.medium"
        case .connection: return "network"
        case .video: return "video"
        case .audio: return "speaker.wave.2"
        case .display: return "display"
        case .input: return "keyboard"
        case .redirection: return "arrow.left.arrow.right"
        case .advanced: return "gearshape.2"
        case .permissions: return "lock.shield"
        }
    }
}

// MARK: - Root view

struct SettingsView: View {
    @ObservedObject var model: SettingsModel

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                // All categories visible at once (System-Settings-style sidebar).
                List(SettingsSection.allCases, selection: Binding(
                    get: { model.section },
                    set: { if let s = $0 { model.section = s } })) { section in
                    Label(section.title, systemImage: section.icon).tag(section)
                }
                .listStyle(.sidebar)
                .frame(width: 176)

                Divider()

                detail.frame(maxWidth: .infinity, maxHeight: .infinity).disabled(model.isApplying)
            }

            Divider()

            if let error = model.applyError ?? model.capabilityError {
                ScrollView {
                    Text(error).font(.caption).foregroundColor(.red).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }.frame(maxHeight: 95).padding(.horizontal, 12).padding(.top, 8)
            }
            if !model.reviewWarnings.isEmpty {
                ScrollView {
                    Text("Review before saving:\n" + model.reviewWarnings.joined(separator: "\n"))
                        .font(.caption).foregroundColor(.orange).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }.frame(maxHeight: 85).padding(.horizontal, 12)
            }

            HStack {
                status
                Spacer()
                Button("Reload") { model.revert() }
                    .disabled(model.isApplying)
                Button(model.reviewWarnings.isEmpty ? (model.serverRunning ? "Apply & restart" : "Save") : "Apply with warnings") {
                    model.apply(acceptWarnings: !model.reviewWarnings.isEmpty)
                }
                    .keyboardShortcut("s", modifiers: .command)
                    .disabled(!model.isDirty || model.isApplying || !model.canApply)
            }
            .padding(12)
        }
        .frame(width: 740, height: 640)
    }

    @ViewBuilder private var detail: some View {
        switch model.section {
        case .status: StatusView(model: model)
        case .connection: ConnectionTab(model: model)
        case .video: VideoTab(model: model)
        case .audio: AudioTab(model: model)
        case .display: DisplayTab(model: model)
        case .input: InputTab(model: model)
        case .redirection: RedirectionTab(model: model)
        case .advanced: AdvancedTab(model: model)
        case .permissions: PermissionsTab(model: model)
        }
    }

    @ViewBuilder private var status: some View {
        if model.isApplying {
            ProgressView().controlSize(.small)
            Text("Validating and applying…").font(.callout)
        } else if let message = model.applyMessage {
            Text(message).font(.caption).foregroundColor(.secondary)
        } else if model.isDirty {
            Label(model.serverRunning ? "Unsaved; applying disconnects remote sessions" : "Unsaved changes",
                  systemImage: "pencil.circle")
                .font(.callout).foregroundColor(.secondary)
        } else {
            Text(model.serverRunning ? "Server running" : "Server stopped")
                .font(.callout).foregroundColor(.secondary)
        }
    }
}

// MARK: - Tabs

private struct ConnectionTab: View {
    @ObservedObject var model: SettingsModel
    var body: some View {
        Form {
            Section("Network") {
                Toggle("Allow connections from the network", isOn: Binding(
                    get: { model.allowNetwork },
                    set: { on in
                        if on {
                            let a = NSAlert()
                            a.messageText = "Allow connections from the network?"
                            a.informativeText = "macrdp will listen on all interfaces (0.0.0.0), so other "
                                + "devices on your network can connect. Access still requires TLS and your "
                                + "macOS account password — enable only on a network you trust."
                            a.addButton(withTitle: "Allow")
                            a.addButton(withTitle: "Cancel")
                            NSApp.activate(ignoringOtherApps: true)
                            guard a.runModal() == .alertFirstButtonReturn else { return }
                        }
                        model.setAllowNetwork(on)
                    }))
                HStack {
                    Text("Listening on")
                    Spacer()
                    Text(model.bindDisplay).foregroundColor(.secondary)
                }
                Text("Off = loopback only (127.0.0.1), reachable from this Mac only.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Account") {
                Button(model.controller.hasKeychainPassword()
                    ? "Change Account Password…" : "Set Account Password…") {
                    model.controller.setPassword()
                }
                Text("Stored in your login Keychain; RDP clients authenticate against your Mac account.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Quick setup") {
                Button("Set Up Remote Desktop preset") { model.applyRemoteDesktopPreset() }
                Text("Stages a headless virtual display + detach the physical panel + H.264 + the "
                    + "app-switcher HUD. Review the Display/Video tabs, then Apply.")
                    .font(.caption).foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}

private struct VideoTab: View {
    @ObservedObject var model: SettingsModel
    var body: some View {
        Form {
            if let overrides = model.displayOverrides {
                Text(overrides).font(.caption).foregroundColor(.orange)
            }
            Section {
                Toggle("H.264 video (EGFX / AVC420)", isOn: model.boolBinding("ENABLE_H264"))
                Text("Streams the screen as H.264 — far less bandwidth than legacy bitmaps. "
                    + "Recommended. Clients without an H.264 decoder fall back automatically.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Bitrate") {
                Stepper(value: Binding(
                    get: { Int(model.bitrateMbps) ?? 6 },
                    set: { model.setBitrate(String($0)) }), in: 1...50) {
                    Text("Ceiling: \(model.bitrateMbps) Mbps")
                }
                .disabled(!model.bool("ENABLE_H264"))
                Toggle("Adaptive bitrate (lower under congestion)", isOn: model.boolBinding("ADAPTIVE_BITRATE"))
                    .disabled(!model.bool("ENABLE_H264"))
                Text("The ceiling is the maximum. With Adaptive on, the server drops below it "
                    + "under congestion (thin/lossy link) and climbs back when it clears; on a "
                    + "clean link it stays at the ceiling. H.264 only.")
                    .font(.caption).foregroundColor(.secondary)
            }
            if !model.bool("VIRTUAL_DISPLAY") { Section {
                Toggle("HiDPI capture (Retina pixels)", isOn: model.boolBinding("HIDPI"))
                Text("Captures the primary display at backing (Retina) resolution — crisper, ~4× the "
                    + "pixels. Best with H.264 and a fast client; mstsc can feel laggy at HiDPI.")
                    .font(.caption).foregroundColor(.secondary)
            } } else {
                Section { Text("Virtual-display scaling is configured in Display. The mirror capture HiDPI setting does not control it.")
                    .font(.caption).foregroundColor(.secondary) }
            }
        }
        .formStyle(.grouped)
    }
}

private struct AudioTab: View {
    @ObservedObject var model: SettingsModel
    var body: some View {
        Form {
            Section {
                Toggle("AAC audio", isOn: model.boolBinding("ENABLE_AAC"))
                Text("Compresses forwarded audio as AAC-LC (~11× less bandwidth than PCM). Clients "
                    + "without AAC fall back to PCM. Adds ~40–50 ms latency, so it's off by default.")
                    .font(.caption).foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}

private struct DisplayTab: View {
    @ObservedObject var model: SettingsModel
    var body: some View {
        Form {
            Section("Virtual display") {
                Toggle("Headless virtual display", isOn: model.boolBinding("VIRTUAL_DISPLAY"))
                Text("Creates a separate Mac display for the remote session. Client requests and macOS display limits determine the actual result; see Status after connecting.")
                    .font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                if let overrides = model.displayOverrides {
                    Text(overrides).font(.caption).foregroundColor(.orange)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                }
                if !model.supportsDisplayPolicy {
                    Text("This server has not advertised support for display policies. Update the server to edit these settings.")
                        .font(.caption).foregroundColor(.orange)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                }
                Group {
                    Picker("Resolution policy", selection: Binding(get: { model.resolutionPolicy }, set: { model.setString("RESOLUTION_POLICY", $0) })) {
                        Text("Follow remote client's request").tag("client")
                        Text("Fixed pixel size").tag("fixed")
                        if !["client", "fixed"].contains(model.resolutionPolicy) { Text("Unknown / invalid").tag(model.resolutionPolicy) }
                    }
                    Picker(model.resolutionPolicy == "fixed" ? "Pixel size" : "Initial / fallback pixel size", selection: Binding(
                        get: { model.resolution }, set: { model.setResolution($0) })) {
                        ForEach(model.resolutionOptions, id: \.0) { value, label in Text(label).tag(value) }
                    }
                    HStack {
                        TextField("Width", text: model.stringBinding("VD_WIDTH", default: "1920"))
                        Text("×")
                        TextField("Height", text: model.stringBinding("VD_HEIGHT", default: "1080"))
                    }
                    Toggle("Allow resolution changes during a session", isOn: Binding(
                        get: { model.resolutionPolicy == "client" && model.bool("LIVE_RESIZE", default: true) },
                        set: { model.setBool("LIVE_RESIZE", $0) }))
                        .disabled(model.resolutionPolicy == "fixed")
                    Picker("Mac interface scaling", selection: Binding(get: { model.virtualScale }, set: { model.setString("VD_HIDPI", $0) })) {
                        Text("100% — standard pixels").tag("0")
                        Text("200% — HiDPI").tag("1")
                        Text("Follow client scale — experimental").tag("auto")
                        if !["0", "1", "auto"].contains(model.virtualScale) { Text("Unknown / invalid").tag(model.virtualScale) }
                    }
                    Text(model.resolutionPolicy == "client"
                         ? "Uses the size requested by RDP, which may differ from the remote monitor. Live changes require client support; that capability is unknown until a request arrives."
                         : "Keeps this Mac display size when clients connect or resize. The client may fit the image to its window.")
                        .font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                    Text("4K at 200% requests a 3840 × 2160 pixel image with a 1920 × 1080 point workspace. Auto chooses supported Mac scaling; it does not promise arbitrary Windows scale factors.")
                        .font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                }.disabled(!model.bool("VIRTUAL_DISPLAY") || !model.supportsDisplayPolicy)
            }
            Section("Primary screen (while connected)") {
                Picker("Physical panel", selection: Binding(
                    get: { model.primaryMode }, set: { model.setPrimaryMode($0) })) {
                    Text("Keep local screen on").tag("none")
                    Text("Detach — move apps to remote").tag("detach")
                    Text("Blank — keep apps on Mac (lockable)").tag("shield")
                    Text("Blank — keep apps on Mac (can't lock)").tag("capture")
                }
                .pickerStyle(.radioGroup)
                Text("Detach/Blank auto-enable the virtual display. “Blank (can't lock)” uses capture — "
                    + "while engaged the Mac CANNOT be locked. “Blank (lockable)” shields with a black "
                    + "window and stays lockable.")
                    .font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
            }
            Section("Lock while away (experimental)") {
                let headless = model.primaryMode != "none"
                Toggle("Lock the Mac when the last client disconnects",
                       isOn: model.boolBinding("LOCK_ON_DISCONNECT"))
                    .disabled(!headless)
                Toggle("Unlock it automatically when a client reconnects",
                       isOn: model.boolBinding("AUTO_UNLOCK"))
                    .disabled(!headless)
                if !headless {
                    Text("Needs one of the Detach/Blank modes above.")
                        .font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                } else if model.bool("LOCK_ON_DISCONNECT") && !model.bool("AUTO_UNLOCK") {
                    Text("Without automatic unlock you'll need to be at the Mac to unlock it — "
                        + "a remote-only Mac would be unreachable once it locks.")
                        .font(.caption).foregroundColor(.orange)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                }
                Text("Locking waits for physical-display restoration and the disconnect safety delay "
                    + "(about 25 s plus restoration time), then requests a password-required lock. "
                    + "The screen-saver password delay is unchanged; failures are recorded in the log. "
                    + "Automatic unlock runs after authentication, before capture, and types your account "
                    + "password — while unlocked, anyone at the Mac has your desktop.")
                    .font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
            }
        }
        .formStyle(.grouped)
    }
}

private struct InputTab: View {
    @ObservedObject var model: SettingsModel
    var body: some View {
        Form {
            Section("Keyboard") {
                Picker("Layout", selection: model.stringBinding("KEYBOARD_LAYOUT", default: "")) {
                    ForEach(AppController.keyboardLayouts, id: \.0) { spec, label in
                        Text(label).tag(spec)
                    }
                }
                Text("Auto-detected from the client by default. Pick one to force it; the Mac's own "
                    + "input source is never changed.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Modifier key positions") {
                Picker("Modifier layout", selection: model.modifierLayoutBinding) {
                    Text("Standard: Windows → Command, Alt → Option").tag("standard")
                    Text("Swap left Windows / Alt").tag("swap-left-win-alt")
                    Text("Swap both Windows / Alt (advanced)").tag("swap-win-alt")
                }
                .disabled(!model.supportsModifierLayout)
                Text(model.modifierLayout == "standard"
                     ? "Ctrl → Control; Windows → Command; Alt → Option."
                     : "Left Ctrl → Control; left Windows → Option; left Alt → Command. "
                       + (model.modifierLayout == "swap-left-win-alt" ? "Right-side keys stay unchanged; this mode does not remap right Alt." : "Right Windows / Alt are also swapped; this can affect AltGr typing."))
                    .font(.caption).foregroundColor(.secondary)
                if !model.supportsModifierLayout {
                    Text("Update the server to a build supporting modifier layouts to change this setting.")
                        .font(.caption).foregroundColor(.secondary)
                }
                Text("Remote maintenance chords keep their physical keys: Ctrl+Alt+G and Ctrl+Alt+Shift+R. Apply restarts the server.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Windows editing compatibility") {
                if model.hasKeyboardOverrides {
                    Text("Advanced extra flags also control keyboard mapping. Remove those overrides before applying these dedicated controls; conflicting settings are rejected.")
                        .font(.caption).foregroundColor(.orange)
                }
                Toggle("Use Ctrl+C/V and other Windows editing shortcuts", isOn: model.boolBinding("MAP_CTRL_TO_CMD"))
                Text("Independent of modifier positions. Converts selected editing shortcuts and Ctrl+click to Command; terminals and excluded apps retain Control.")
                    .font(.caption).foregroundColor(.secondary)
                if model.bool("MAP_CTRL_TO_CMD") {
                    ExcludeAppsView(model: model)
                }
            }
            Section("Mouse") {
                Toggle("Reverse scroll direction", isOn: model.boolBinding("REVERSE_SCROLL"))
                Text("For a Mac with natural scrolling on, if remote scrolling comes out backwards.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("App switching") {
                Toggle("Option+Tab switches apps (\(model.optionKeyHint)+Tab)", isOn: model.boolBinding("ALT_TAB_SWITCH"))
                Toggle("Option+Q quits apps (\(model.optionKeyHint)+Q)", isOn: model.boolBinding("ALT_Q_QUIT"))
                if model.modifierLayout != "standard" {
                    Text("Left Alt+Tab already sends Command+Tab. The Option shortcuts above use the Windows key after swapping. Windows may intercept some combinations before RDP receives them.")
                        .font(.caption).foregroundColor(.secondary)
                }
                Toggle("App-switcher HUD overlay", isOn: model.boolBinding("APP_SWITCHER_HUD"))
                Toggle("Un-minimize on Cmd+Tab", isOn: model.boolBinding("UNMINIMIZE"))
            }
        }
        .formStyle(.grouped)
    }
}

private struct ExcludeAppsView: View {
    @ObservedObject var model: SettingsModel
    var body: some View {
        let custom = model.excludeList().filter { bundle in
            !AppController.remapExcludeApps.contains { $0.0 == bundle }
        }
        Text("Keep Ctrl unmapped in these apps (e.g. editors with an embedded terminal, so Ctrl+C "
            + "stays SIGINT). Standalone terminals are auto-excluded.")
            .font(.caption).foregroundColor(.secondary)
        ForEach(AppController.remapExcludeApps, id: \.0) { bundle, label in
            Toggle(label, isOn: Binding(
                get: { model.isExcluded(bundle) },
                set: { model.toggleExclude(bundle, $0) }))
        }
        ForEach(custom, id: \.self) { bundle in
            Toggle(bundle, isOn: Binding(
                get: { model.isExcluded(bundle) },
                set: { model.toggleExclude(bundle, $0) }))
        }
        HStack {
            Button("Add app…") { model.addExcludeApp() }
            if !model.excludeList().isEmpty {
                Button("Clear list") { model.setExcludeList([]) }
            }
        }
    }
}

private struct RedirectionTab: View {
    @ObservedObject var model: SettingsModel
    /// Bumped after an install/remove so the driver status is re-read.
    @State private var micRefresh = 0
    var body: some View {
        Form {
            Text("The connecting client must also opt in to each redirection.")
                .font(.caption).foregroundColor(.secondary)
            Section("Drive") {
                Toggle("Drive redirection", isOn: model.boolBinding("ENABLE_DRIVE_REDIRECTION"))
                Text("The client's drive mounts on the Mac as a real volume (read-write).")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Smart card") {
                Toggle("Smart-card redirection", isOn: model.boolBinding("ENABLE_SMARTCARD_REDIRECTION"))
                Button("Install smart-card handler…") { model.controller.installSmartcardHandler() }
                Text("The handler must be installed once (needs admin + a USB trigger device).")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Camera") {
                Toggle("Camera redirection", isOn: model.boolBinding("ENABLE_CAMERA_REDIRECTION"))
                Button("Enable macrdp Camera…") { model.controller.enableCameraRedirection() }
                Text("Presents the client's webcam as a “macrdp Camera” in Photo Booth / Zoom / "
                    + "FaceTime. The system extension must be enabled once.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Microphone") {
                let status: AppController.MicDriverStatus = {
                    _ = micRefresh
                    return model.controller.micDriverStatus()
                }()
                Toggle("Microphone redirection", isOn: model.boolBinding("ENABLE_MICROPHONE_REDIRECTION"))
                HStack {
                    Button(status == .updateAvailable ? "Update macrdp Microphone…"
                               : "Install macrdp Microphone…") {
                        model.controller.installMicDriver()
                        micRefresh += 1
                    }
                    .disabled(status == .installed)
                    if status != .notInstalled {
                        Button("Remove…") {
                            model.controller.removeMicDriver()
                            micRefresh += 1
                        }
                    }
                }
                Text(status == .notInstalled ? "Driver: not installed"
                        : status == .updateAvailable ? "Driver: installed — this macrdp bundles a newer one"
                        : "Driver: installed")
                    .font(.caption)
                Text("Presents the client's microphone as “macrdp Microphone” in Zoom / FaceTime / "
                    + "QuickTime. The driver must be installed once (admin; Core Audio restarts, "
                    + "briefly interrupting sound). Other accounts on this Mac can listen to a live "
                    + "stream.")
                    .font(.caption).foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}

private struct AdvancedTab: View {
    @ObservedObject var model: SettingsModel
    var body: some View {
        Form {
            Section("UDP multitransport (experimental)") {
                Toggle("Offer UDP multitransport", isOn: model.boolBinding("ENABLE_UDP_MULTITRANSPORT"))
                Toggle("Move video onto UDP (clean link only)", isOn: model.boolBinding("UDP_MIGRATE_EGFX"))
                    .disabled(!model.bool("ENABLE_UDP_MULTITRANSPORT"))
                Text("Moving video onto the reliable UDP tunnel helps on a clean, low-latency link, "
                    + "but under packet loss the picture can freeze until reconnect (audio stays on "
                    + "TCP). Auto-enables H.264. LAN/Wi-Fi only.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Diagnostics") {
                Toggle("Live statistics endpoint", isOn: model.boolBinding("STATS_ENDPOINT"))
                Text("Lets the Status tab show live video bitrate, link RTT and frame-rate. "
                    + "Loopback-only (127.0.0.1), read-only, no disk writes. Takes effect after Apply.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section("Extra flags") {
                TextField("Extra flags", text: model.stringBinding("EXTRA_FLAGS"), prompt: Text("e.g. --bitrate 6"))
                Text("Passed verbatim to the server for anything not covered above.")
                    .font(.caption).foregroundColor(.secondary)
            }
            Section {
                Button("Edit config.env directly…") { model.controller.editConfig() }
                Button("Open Logs") { model.controller.openLogs() }
            }
        }
        .formStyle(.grouped)
    }
}

private struct PermissionsTab: View {
    @ObservedObject var model: SettingsModel
    @State private var screen: Bool?
    @State private var accessibility: Bool?
    @State private var statusNote = "Reading the server's startup permission checks…"

    var body: some View {
        Form {
            Text("macrdp needs these grants (owned by the server binary, not this controller). "
                + "These are startup checks, not live TCC status.")
                .font(.caption).foregroundColor(.secondary)
            Text(statusNote).font(.caption).foregroundColor(.secondary)
            Section("Screen Recording") {
                permRow(granted: screen) { model.controller.openScreenRecording() }
            }
            Section("Accessibility") {
                permRow(granted: accessibility) { model.controller.openAccessibility() }
            }
            Button("Refresh status") { refresh() }
        }
        .formStyle(.grouped)
        .onAppear(perform: refresh)
    }

    private func refresh() {
        let controller = model.controller
        DispatchQueue.global(qos: .utility).async {
            let result = Result { () throws -> DisplaySnapshot in
                let snapshot = try controller.displaySnapshot()
                guard let pid = controller.agentState().pid, snapshot.pid == pid else {
                    throw ConfigError.invalid("No permission checks from the current server process.")
                }
                return snapshot
            }
            DispatchQueue.main.async {
                switch result {
                case .success(let snapshot):
                    screen = snapshot.startup["screen_recording"] as? Bool
                    accessibility = snapshot.startup["accessibility"] as? Bool
                    statusNote = "Reported by the current server at startup. Restart after changing permissions to recheck."
                case .failure(let error):
                    screen = nil; accessibility = nil; statusNote = error.localizedDescription
                }
            }
        }
    }

    @ViewBuilder private func permRow(granted: Bool?, open: @escaping () -> Void) -> some View {
        HStack {
            switch granted {
            case .some(true):
                Label("Granted at startup", systemImage: "checkmark.circle.fill").foregroundColor(.green)
            case .some(false):
                Label("Not granted at startup", systemImage: "xmark.circle.fill").foregroundColor(.red)
            case .none:
                Label("Unknown — open to grant", systemImage: "questionmark.circle").foregroundColor(.secondary)
            }
            Spacer()
            Button("Open System Settings…", action: open)
        }
    }
}

// MARK: - Status (live, read-only)

/// Process-level stats sampled from `ps` (no server change needed).
struct ServerStats {
    let running: Bool
    let pid: Int
    let uptime: String
    let rssMB: Double
    let cpu: Double
    let memPct: Double
    static let stopped = ServerStats(running: false, pid: 0, uptime: "", rssMB: 0, cpu: 0, memPct: 0)
}

/// An observed TCP peer; it may still be authenticating and has no verified identity.
struct ConnectionInfo {
    let connected: Bool
    let ip: String?
    static let none = ConnectionInfo(connected: false, ip: nil)
}

/// Live H.264 telemetry from the server's opt-in loopback stats endpoint.
struct LiveStats {
    let bitrateBps: Int
    let ceilingBps: Int
    let rttMs: Int
    let queueMs: Int
    let fps: Int
    let frames: Int
    let adaptive: Bool
}

private struct StatusView: View {
    @ObservedObject var model: SettingsModel
    @State private var stats: ServerStats = .stopped
    @State private var conn: ConnectionInfo = .none
    @State private var live: LiveStats?
    @State private var displayStatus: DisplaySnapshot?
    @State private var displayError: String?
    @State private var refreshing = false
    // Only ticks while this pane is on screen (subscription cancels on disappear).
    private let tick = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    var body: some View {
        Form {
            Section("Server") {
                row("Status", stats.running ? "Running (pid \(stats.pid))" : "Stopped")
                if stats.running {
                    row("Uptime", stats.uptime.isEmpty ? "—" : stats.uptime)
                    row("Memory", String(format: "%.0f MB  (%.1f%% of RAM)", stats.rssMB, stats.memPct))
                    row("CPU", String(format: "%.1f%%", stats.cpu))
                }
            }
            Section("Remote display — reported by server") {
                if let d = displayStatus {
                    row("Server startup", d.startup["phase"] as? String ?? "Unknown")
                    row("Display state", d.phase ?? "Unknown")
                    row("Client-requested pixels", d.dimensions("requested"))
                    row("Client-requested scale", d.scale("requested"))
                    row("RDP output pixels", d.dimensions("wire"))
                    row("Mac workspace (points)", d.dimensions("actual", width: "points_width", height: "points_height"))
                    row("Mac backing pixels", d.dimensions("actual", width: "backing_width", height: "backing_height"))
                    row("Remote interface scale", d.scale("actual"))
                    row("Mac rendering scale", d.scale("actual", key: "backing_scale_percent"))
                    row("Native pixel capture", ((d.display["actual"] as? [String: Any])?["native_capture"] as? Bool).map { $0 ? "Yes" : "No" } ?? "Unknown")
                    if d.phase == "idle" {
                        Text("No display request has been applied in this session.").font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                    }
                    if d.phase == "degraded", let reason = d.display["fallback_reason"] as? String {
                        Text(reason).font(.caption).foregroundColor(.orange)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                    }
                } else {
                    Text(displayError ?? "Waiting for status…").font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                    Text("Requested settings are not proof of the active display mode.").font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                }
            }
            Section("Connection") {
                if let connected = displayStatus?.object["connected"] as? Bool {
                    row("RDP session", connected ? "Connected (server reported)" : "Disconnected (server reported)")
                    row("Client identity", "Unknown")
                } else {
                    Text(conn.connected ? "TCP connection observed; RDP session is unverified." : "No TCP connection observed; RDP session status is unknown.")
                        .foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                }
                if let ip = conn.ip { row("Observed TCP peer", ip) }
                Text("Client support for live resolution changes is unknown until it sends a request.")
                    .font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
            }
            if displayStatus?.object["connected"] as? Bool == true {
                Section("Video (H.264)") {
                    if let l = live {
                        row("Bitrate", bitrateText(l))
                        row("Frame rate", "\(l.fps) fps")
                        if l.rttMs > 0 { row("Link RTT", "\(l.rttMs) ms") }
                        if l.adaptive { row("Standing queue", "\(l.queueMs) ms") }
                        row("Frames sent", "\(l.frames)")
                    } else {
                        Text("Turn on “Live statistics endpoint” in Advanced (then Apply) to see "
                            + "bitrate, link RTT and frame-rate here. Shown only over H.264.")
                            .font(.caption).foregroundColor(.secondary)
                        .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear(perform: refresh)
        .onReceive(tick) { _ in refresh() }
    }

    private func refresh() {
        guard !refreshing else { return }
        refreshing = true
        // Gather off the main thread — serverStats/currentConnection/liveStats each
        // shell out (launchctl/ps/lsof/nc); doing that on the main thread every 2 s
        // would jank the UI. Publish the results back on main.
        let controller = model.controller
        DispatchQueue.global(qos: .utility).async {
            let s = controller.serverStats()
            let c = controller.currentConnection()
            let display = Result { try controller.displaySnapshot() }
            DispatchQueue.main.async {
                stats = s
                conn = c
                refreshing = false
                switch display {
                case .success(let snapshot) where snapshot.pid == s.pid && s.running:
                    displayStatus = snapshot; displayError = nil
                    live = controller.liveStats(from: snapshot.object)
                case .success:
                    displayStatus = nil; live = nil; displayError = "Display status belongs to a different process; waiting for the current server."
                case .failure(let error):
                    displayStatus = nil; live = nil; displayError = error.localizedDescription
                }
            }
        }
    }

    private func bitrateText(_ l: LiveStats) -> String {
        let mbps = Double(l.bitrateBps) / 1_000_000
        if l.adaptive, l.ceilingBps > 0 {
            return String(format: "%.1f Mbps  (ceiling %.0f)", mbps, Double(l.ceilingBps) / 1_000_000)
        }
        return String(format: "%.1f Mbps", mbps)
    }

    private func row(_ key: String, _ value: String) -> some View {
        HStack {
            Text(key)
            Spacer()
            Text(value).foregroundColor(.secondary).textSelection(.enabled)
        }
    }
}

// MARK: - Stats gathering + About (controller-side, no server change)

extension AppController {
    /// Live RSS / CPU / uptime of the server process (via `ps`).
    func serverStats() -> ServerStats {
        guard let pid = agentState().pid else { return .stopped }
        let out = run("/bin/ps", ["-o", "rss=,%cpu=,etime=,%mem=", "-p", String(pid)])
        let f = out.stdout.split(whereSeparator: { $0 == " " || $0 == "\n" || $0 == "\t" }).map(String.init)
        guard f.count >= 4 else {
            return ServerStats(running: true, pid: pid, uptime: "—", rssMB: 0, cpu: 0, memPct: 0)
        }
        return ServerStats(
            running: true, pid: pid, uptime: f[2],
            rssMB: (Double(f[0]) ?? 0) / 1024, cpu: Double(f[1]) ?? 0, memPct: Double(f[3]) ?? 0)
    }

    /// An observed TCP peer, not proof of an authenticated RDP session.
    func currentConnection() -> ConnectionInfo {
        guard let pid = agentState().pid else { return .none }
        let port = (readConfig()["BIND"] ?? "127.0.0.1:3390").split(separator: ":").last.map(String.init) ?? "3390"
        let out = run("/usr/sbin/lsof", ["-nP", "-a", "-p", String(pid), "-iTCP", "-sTCP:ESTABLISHED"])
        var ip: String?
        for raw in out.stdout.split(separator: "\n") {
            let line = String(raw)
            guard let arrow = line.range(of: "->") else { continue }
            // The RDP client connection is the one whose LOCAL endpoint is the RDP
            // port (loopback helper channels have other local ports).
            guard line[..<arrow.lowerBound].hasSuffix(":\(port)") else { continue }
            let remote = line[arrow.upperBound...].split(whereSeparator: { $0.isWhitespace }).first ?? ""
            if let colon = remote.lastIndex(of: ":") {
                let host = String(remote[..<colon]).trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
                if !host.isEmpty { ip = host; break }
            }
        }
        guard let clientIP = ip else { return .none }
        return ConnectionInfo(connected: true, ip: clientIP)
    }

    /// H.264 telemetry from the same process-checked status snapshot as display state.
    func liveStats(from o: [String: Any]) -> LiveStats? {
        guard (o["frames_sent"] as? NSNumber)?.intValue ?? 0 > 0 else { return nil }
        func int(_ k: String) -> Int { (o[k] as? NSNumber)?.intValue ?? 0 }
        func bool(_ k: String) -> Bool { (o[k] as? NSNumber)?.boolValue ?? false }
        return LiveStats(
            bitrateBps: int("bitrate_bps"), ceilingBps: int("ceiling_bps"),
            rttMs: int("rtt_ms"), queueMs: int("queue_delay_ms"),
            fps: int("fps"), frames: int("frames_sent"), adaptive: bool("adaptive"))
    }

    /// The standard macOS About panel, populated with the app version + a short
    /// description + the live server status.
    @objc func showAbout() {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let st = agentState()
        let statusLine: String
        if let pid = st.pid { statusLine = "Server: running (pid \(pid))" }
        else if st.loaded { statusLine = "Server: installed, stopped" }
        else { statusLine = "Server: not installed" }
        let body = "Native RDP server for macOS.\n\n"
            + "RDP clients connect over TLS to your Mac and get the desktop with keyboard, "
            + "mouse, clipboard and audio — plus optional H.264 video, headless virtual "
            + "displays, and drive / smart-card / camera / USB redirection.\n\n"
            + statusLine
        let opts: [NSApplication.AboutPanelOptionKey: Any] = [
            .applicationName: "macrdp",
            .applicationVersion: version,
            .credits: NSAttributedString(
                string: body,
                attributes: [.font: NSFont.systemFont(ofSize: 11), .foregroundColor: NSColor.labelColor]),
        ]
        NSApp.orderFrontStandardAboutPanel(options: opts)
    }
}
