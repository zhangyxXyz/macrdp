import Foundation

/// The server's quote/escape grouping grammar. No shell evaluation or expansion.
public struct ExtraFlags {
    public let tokens: [String]
    public init(_ text: String) throws {
        var result: [String] = [], token = ""
        var quote: Character?, escape = false, started = false
        for ch in text {
            if escape { token.append(ch); escape = false; started = true; continue }
            if ch == "\\" && quote != "'" { escape = true; started = true; continue }
            if let q = quote {
                if ch == q { quote = nil } else { token.append(ch) }
            } else if ch == "'" || ch == "\"" { quote = ch; started = true }
            else if ch.isWhitespace {
                if started { result.append(token); token = ""; started = false }
            } else { token.append(ch); started = true }
        }
        guard quote == nil && !escape else { throw ConfigError.invalid("EXTRA_FLAGS has an unmatched quote or trailing escape.") }
        if started { result.append(token) }
        tokens = result
    }
    public func contains(_ flag: String) -> Bool { tokens.contains { $0 == flag || $0.hasPrefix(flag + "=") } }
    public func value(_ flag: String) -> String? {
        guard let index = tokens.lastIndex(where: { $0 == flag || $0.hasPrefix(flag + "=") }) else { return nil }
        let token = tokens[index]
        if token.hasPrefix(flag + "=") { return String(token.dropFirst(flag.count + 1)) }
        guard index + 1 < tokens.count, !tokens[index + 1].hasPrefix("--") else { return nil }
        return tokens[index + 1]
    }
}
