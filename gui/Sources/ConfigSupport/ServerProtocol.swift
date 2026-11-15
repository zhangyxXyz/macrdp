import Foundation

public struct ServerCapabilities {
    public let features: Set<String>
    public let keys: Set<String>
    public let buildCommit: String?
    public init(data: Data) throws {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["schema_version"] as? Int == 1 else {
            throw ConfigError.invalid("The installed server does not provide a supported capabilities response.")
        }
        features = Set(object["features"] as? [String] ?? [])
        keys = Set(object["supported_config_keys"] as? [String] ?? [])
        buildCommit = object["build_commit"] as? String
    }
    public var canValidate: Bool { features.contains("config_validation") }
    public var canConfigureDisplay: Bool {
        canValidate && keys.isSuperset(of: ["RESOLUTION_POLICY", "LIVE_RESIZE", "VD_HIDPI"])
    }
}

public struct ConfigValidation {
    public let revision: String
    public let warnings: [String]
    public init(data: Data) throws {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["schema_version"] as? Int == 1 else {
            throw ConfigError.invalid("The server returned an unsupported validation response.")
        }
        guard object["ok"] as? Bool == true else {
            let errors = object["errors"] as? [String] ?? ["Configuration validation failed."]
            throw ConfigError.invalid(errors.joined(separator: "\n"))
        }
        guard let revision = object["config_revision"] as? String, !revision.isEmpty else {
            throw ConfigError.invalid("The server did not identify the validated configuration.")
        }
        self.revision = revision
        warnings = object["warnings"] as? [String] ?? []
    }
}

/// Missing fields intentionally remain nil. Zero is not a substitute for unknown.
public struct DisplaySnapshot {
    public let object: [String: Any]
    public init(data: Data) throws {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["schema_version"] as? Int == 2 else {
            throw ConfigError.invalid("Display status is unavailable from this server version.")
        }
        self.object = object
    }
    public var display: [String: Any] { object["display"] as? [String: Any] ?? [:] }
    public var startup: [String: Any] { object["startup"] as? [String: Any] ?? [:] }
    public var pid: Int? { (object["process"] as? [String: Any])?["pid"] as? Int }
    public var revision: String? { object["config_revision"] as? String }
    public var phase: String? { display["phase"] as? String }
    public func value(_ group: String, _ key: String) -> Double? {
        ((display[group] as? [String: Any])?[key] as? NSNumber)?.doubleValue
    }
    public func dimensions(_ group: String, width: String = "width", height: String = "height") -> String {
        guard let w = value(group, width), let h = value(group, height) else { return "Unknown" }
        return String(format: "%.0f × %.0f", w, h)
    }
    public func scale(_ group: String, key: String = "scale_percent") -> String {
        guard let scale = value(group, key) else { return "Unknown" }
        return String(format: "%.0f%%", scale)
    }
}
