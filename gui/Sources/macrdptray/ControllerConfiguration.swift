import AppKit
import ConfigSupport

extension AppController {
    var configurationBinary: URL? {
        if let data = try? Data(contentsOf: plistURL),
           let plist = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
           let args = plist["ProgramArguments"] as? [String], let path = args.first {
            return URL(fileURLWithPath: path)
        }
        return locateServerApp()?.appendingPathComponent("Contents/MacOS/macrdp")
    }

    /// Bounded subprocess with file-backed output, avoiding a full pipe deadlock.
    func runConfigurationCommand(_ path: String, _ args: [String], timeout: TimeInterval = 8) throws -> (Int32, Data) {
        let output = FileManager.default.temporaryDirectory.appendingPathComponent("macrdp-command-\(UUID().uuidString)")
        FileManager.default.createFile(atPath: output.path, contents: nil, attributes: [.posixPermissions: 0o600])
        defer { try? FileManager.default.removeItem(at: output) }
        let file = try FileHandle(forWritingTo: output)
        defer { try? file.close() }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = args
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = file
        process.standardError = file
        try process.run()
        let deadline = Date().addingTimeInterval(timeout)
        while process.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
        if process.isRunning {
            process.terminate()
            let grace = Date().addingTimeInterval(0.5)
            while process.isRunning && Date() < grace { Thread.sleep(forTimeInterval: 0.02) }
            if process.isRunning { kill(process.processIdentifier, SIGKILL) }
            throw ConfigError.invalid("Timed out while running \(URL(fileURLWithPath: path).lastPathComponent).")
        }
        process.waitUntilExit()
        let data = try Data(contentsOf: output)
        return (process.terminationStatus, Data(data.suffix(64 * 1024)))
    }

    func serverCapabilities() throws -> ServerCapabilities {
        guard let binary = configurationBinary else { throw ConfigError.invalid("macrdp server not found.") }
        let (code, data) = try runConfigurationCommand(binary.path, ["--capabilities"])
        guard code == 0 else { throw ConfigError.invalid("This server cannot validate these settings. Update the server before editing them here.") }
        return try ServerCapabilities(data: data)
    }

    func validateConfiguration(_ url: URL) throws -> ConfigValidation {
        guard let binary = configurationBinary else { throw ConfigError.invalid("macrdp server not found.") }
        let (code, data) = try runConfigurationCommand(binary.path, ["--check-config", url.path])
        let validation = try ConfigValidation(data: data)
        guard code == 0 else { throw ConfigError.invalid("Configuration validation exited with code \(code).") }
        return validation
    }

    func displaySnapshot(config: [String: String]? = nil) throws -> DisplaySnapshot {
        let cfg = config ?? readConfig()
        guard cfg["STATS_ENDPOINT"] == "1" else {
            throw ConfigError.invalid("Display status unavailable: enable Live statistics endpoint in Advanced.")
        }
        guard let port = UInt16(cfg["STATS_PORT"] ?? "40245"), port > 0 else {
            throw ConfigError.invalid("Invalid statistics port.")
        }
        let (code, data) = try runConfigurationCommand("/usr/bin/nc", ["-w", "1", "127.0.0.1", String(port)], timeout: 2)
        guard code == 0 else { throw ConfigError.invalid("Display status unavailable: the local endpoint is not responding.") }
        return try DisplaySnapshot(data: data)
    }

    /// Runs only after candidate validation and atomic replacement. It never authenticates
    /// as an RDP user or sends RDP traffic. A listening port alone is not display readiness.
    func restartAndVerifyConfiguration() throws -> String {
        let expected = try validateConfiguration(configURL)
        let cfg = readConfig()
        let oldPID = agentState().pid
        let (code, output) = try runConfigurationCommand("/bin/launchctl", ["kickstart", "-k", service])
        guard code == 0 else { throw ConfigError.invalid("Restart failed: \(String(decoding: output, as: UTF8.self))") }
        let deadline = Date().addingTimeInterval(12)
        var stablePID: Int?
        var stableSince = Date()
        var lastStatus = "Server did not start."
        while Date() < deadline {
            let pid = agentState().pid
            if let pid = pid, pid != oldPID {
                if stablePID != pid { stablePID = pid; stableSince = Date() }
                if cfg["STATS_ENDPOINT"] == "1" {
                    if let snapshot = try? displaySnapshot(config: cfg), snapshot.pid == pid,
                       snapshot.revision == expected.revision {
                        switch snapshot.startup["phase"] as? String {
                        case "ready": return "Settings saved; server startup verified. Remote display is verified separately after connection."
                        case "permission_required": return "Settings saved; server restarted, but screen recording or accessibility permission is required."
                        case "failed": throw ConfigError.invalid("Server reported startup failure.")
                        default: lastStatus = "Server is still starting."
                        }
                    } else { lastStatus = "Could not verify the new process and configuration through its status endpoint." }
                } else if Date().timeIntervalSince(stableSince) >= 1 {
                    return "Settings saved; process restarted. Runtime display unverified (statistics endpoint is off)."
                }
            }
            Thread.sleep(forTimeInterval: 0.2)
        }
        throw ConfigError.invalid(lastStatus + " See server logs for details.")
    }
}
