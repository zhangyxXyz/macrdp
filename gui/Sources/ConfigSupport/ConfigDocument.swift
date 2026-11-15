import Foundation

/// The same plain key=value grammar as the server; never shell-evaluate a file.
public struct ConfigDocument {
    public let data: Data
    public let values: [String: String]
    private let lines: [String]

    public init(data: Data) throws {
        guard let text = String(data: data, encoding: .utf8) else {
            throw ConfigError.invalid("Configuration must be UTF-8.")
        }
        self.data = data
        self.lines = text.components(separatedBy: "\n")
        var values: [String: String] = [:]
        for line in lines {
            if let (key, value) = Self.assignment(line) { values[key] = value }
        }
        self.values = values
    }

    private static func assignment(_ raw: String) -> (String, String)? {
        var line = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !line.isEmpty, !line.hasPrefix("#") else { return nil }
        if line.hasPrefix("export"), line.dropFirst(6).first?.isWhitespace == true {
            line = String(line.dropFirst(6)).trimmingCharacters(in: .whitespaces)
        }
        guard let eq = line.firstIndex(of: "=") else { return nil }
        let key = String(line[..<eq]).trimmingCharacters(in: .whitespaces)
        var value = String(line[line.index(after: eq)...]).trimmingCharacters(in: .whitespaces)
        if value.count >= 2, let first = value.first,
           (first == "\"" || first == "'"), value.last == first {
            value = String(value.dropFirst().dropLast())
        }
        return (key, value)
    }

    /// Only changed keys are normalized; comments and unrelated/unknown keys stay intact.
    /// All occurrences of a changed key are replaced by one authoritative assignment.
    public func replacing(_ changes: [String: String]) throws -> Data {
        for (key, value) in changes {
            guard !key.isEmpty, !key.contains("="),
                  !key.contains(where: { $0.isWhitespace }),
                  !value.contains("\n"), !value.contains("\r") else {
                throw ConfigError.invalid("Invalid key or multiline value: \(key)")
            }
        }
        var written = Set<String>()
        var result: [String] = []
        for raw in lines {
            if let (key, _) = Self.assignment(raw), let value = changes[key] {
                if written.insert(key).inserted { result.append("\(key)=\(value)") }
            } else { result.append(raw) }
        }
        while result.last == "" { result.removeLast() }
        for key in changes.keys.sorted() where !written.contains(key) {
            result.append("\(key)=\(changes[key]!)")
        }
        return Data((result.joined(separator: "\n") + "\n").utf8)
    }
}

public enum ConfigError: LocalizedError {
    case invalid(String)
    case externalChange
    case activationFailed(String, rollback: String)
    public var errorDescription: String? {
        switch self {
        case .invalid(let message): return message
        case .externalChange: return "Configuration changed outside this window. Reload before applying."
        case .activationFailed(let message, let rollback):
            return "New settings could not be verified: \(message)\n\(rollback)"
        }
    }
}

public enum ConfigTransaction {
    /// Create sensitive content with restrictive permissions from the first byte.
    private static func privateFile(_ data: Data, at url: URL, permissions: Int) throws {
        let fd = open(url.path, O_WRONLY | O_CREAT | O_EXCL, mode_t(permissions & 0o600))
        guard fd >= 0 else { throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno)) }
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        do {
            try handle.write(contentsOf: data)
            try handle.synchronize()
            try handle.close()
        } catch {
            try? handle.close()
            try? FileManager.default.removeItem(at: url)
            throw error
        }
    }

    private static func atomicPrivateWrite(_ data: Data, to url: URL, permissions: Int) throws {
        let temp = url.deletingLastPathComponent().appendingPathComponent(".macrdp-recovery-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: temp) }
        try privateFile(data, at: temp, permissions: permissions)
        guard rename(temp.path, url.path) == 0 else {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
        }
    }
    /// Validate a private candidate before replacing the live file once. Activation is
    /// injectable, so tests never restart a service. A failed activation restores the exact
    /// previous bytes and invokes activation again for that configuration.
    @discardableResult
    public static func apply(url: URL, expected: Data, changes: [String: String],
                             validate: (URL) throws -> Void,
                             activate: () throws -> Void) throws -> Data {
        // Serialize controller instances. Ordinary editors do not take this lock,
        // so compare the live bytes again immediately before each replacement.
        let lockURL = url.deletingLastPathComponent().appendingPathComponent(".macrdp-config.lock")
        let lock = open(lockURL.path, O_WRONLY | O_CREAT | O_NOFOLLOW, mode_t(0o600))
        guard lock >= 0 else { throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno)) }
        defer { flock(lock, LOCK_UN); close(lock) }
        guard flock(lock, LOCK_EX | LOCK_NB) == 0 else {
            throw ConfigError.invalid("Another controller is applying settings. Retry after it finishes.")
        }
        let old = try Data(contentsOf: url)
        guard old == expected else { throw ConfigError.externalChange }
        let candidate = try ConfigDocument(data: old).replacing(changes)
        let fm = FileManager.default
        let permissions = (try fm.attributesOfItem(atPath: url.path)[.posixPermissions] as? NSNumber)?.intValue ?? 0o600
        let staged = url.deletingLastPathComponent().appendingPathComponent(".macrdp-candidate-\(UUID().uuidString)")
        defer { try? fm.removeItem(at: staged) }
        try privateFile(candidate, at: staged, permissions: permissions)
        try validate(staged)
        guard try Data(contentsOf: url) == expected else { throw ConfigError.externalChange }
        // Keep a disk recovery copy even if activation or rollback is interrupted.
        let recovery = url.appendingPathExtension("previous")
        try atomicPrivateWrite(old, to: recovery, permissions: permissions)
        guard try Data(contentsOf: url) == expected else { throw ConfigError.externalChange }
        // POSIX rename is atomic on this filesystem and preserves staged permissions.
        guard rename(staged.path, url.path) == 0 else {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
        }
        do { try activate() }
        catch {
            let failure = error.localizedDescription
            guard (try? Data(contentsOf: url)) == candidate else {
                throw ConfigError.activationFailed(failure, rollback: "Configuration changed again; it was not overwritten. The previous file is at \(recovery.path).")
            }
            do {
                try atomicPrivateWrite(old, to: url, permissions: permissions)
                try activate()
            } catch {
                throw ConfigError.activationFailed(failure, rollback: "Rollback could not be verified: \(error.localizedDescription). Recovery copy: \(recovery.path)")
            }
            throw ConfigError.activationFailed(failure, rollback: "Previous configuration restored; restart completed. Check Status for runtime readiness.")
        }
        return candidate
    }
}
