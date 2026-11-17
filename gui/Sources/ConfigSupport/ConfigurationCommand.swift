import Foundation

/// Finite service cleanup is separate from command inspection and startup readiness.
public enum ServiceTiming {
    public static let agentExitTimeout = 60
    public static let restartCommandTimeout: TimeInterval = 75
    public static let startupVerificationTimeout: TimeInterval = 12
}

public enum ConfigurationCommand {
    /// File-backed output avoids a full-pipe deadlock. Timeout uses a monotonic clock.
    public static func run(_ path: String, _ args: [String], timeout: TimeInterval = 8) throws -> (Int32, Data) {
        let output = FileManager.default.temporaryDirectory.appendingPathComponent("macrdp-command-\(UUID().uuidString)")
        guard FileManager.default.createFile(atPath: output.path, contents: nil, attributes: [.posixPermissions: 0o600]) else {
            throw ConfigError.invalid("Could not prepare command output.")
        }
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
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        while process.isRunning && ProcessInfo.processInfo.systemUptime < deadline { Thread.sleep(forTimeInterval: 0.05) }
        if process.isRunning {
            process.terminate()
            let grace = ProcessInfo.processInfo.systemUptime + 0.5
            while process.isRunning && ProcessInfo.processInfo.systemUptime < grace { Thread.sleep(forTimeInterval: 0.02) }
            if process.isRunning { kill(process.processIdentifier, SIGKILL) }
            process.waitUntilExit()
            throw ConfigError.invalid("Timed out while running \(URL(fileURLWithPath: path).lastPathComponent).")
        }
        process.waitUntilExit()
        let reader = try FileHandle(forReadingFrom: output)
        defer { try? reader.close() }
        let size = try reader.seekToEnd()
        try reader.seek(toOffset: size > 64 * 1024 ? size - 64 * 1024 : 0)
        return (process.terminationStatus, try reader.readToEnd() ?? Data())
    }
}
