import AppKit
import Darwin
import FileProvider
import Foundation

private enum HostError: Error { case usage, invalid, missing }
private struct Command: Decodable {
  let instance: String
  var port: UInt16?
  var token: String?
  var pid: Int32?
  var items: [String]?
}
private func output(_ value: [String: Any]) {
  if let bytes = try? JSONSerialization.data(withJSONObject: value, options: .sortedKeys) {
    FileHandle.standardOutput.write(bytes + Data([10]))
  }
}
private let prefix = (Bundle.main.bundleIdentifier ?? "com.clintcan.macrdp.files") + ".clipboard."
private func allDomains() async throws -> [NSFileProviderDomain] {
  try await withCheckedThrowingContinuation { continuation in
    NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
      if let error {
        continuation.resume(throwing: error)
      } else {
        continuation.resume(returning: domains)
      }
    }
  }
}
private func remove(_ domain: NSFileProviderDomain) async throws {
  try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
    NSFileProviderManager.remove(domain) { error in
      if let error { continuation.resume(throwing: error) } else { continuation.resume() }
    }
  }
}
@main
struct FilesHost {
  static func main() {
    let application = NSApplication.shared
    application.setActivationPolicy(.accessory)
    let operation = CommandLine.arguments.dropFirst().first ?? "status"
    Task {
      do {
        if operation == "status" {
          let domains = try await allDomains().filter { $0.identifier.rawValue.hasPrefix(prefix) }
          output([
            "domains": domains.map {
              ["id": $0.identifier.rawValue, "disconnected": $0.isDisconnected]
            }
          ])
          exit(0)
        }
        let input = FileHandle.standardInput.readDataToEndOfFile()
        guard input.count <= 512 * 1024 else { throw HostError.invalid }
        let command = try JSONDecoder().decode(Command.self, from: input)
        guard UUID(uuidString: command.instance) != nil else { throw HostError.invalid }
        let id = prefix + command.instance
        var domains = try await allDomains()
        if operation == "resolve" || operation == "stop" {
          for _ in 0..<10 where !domains.contains(where: { $0.identifier.rawValue == id }) {
            try await Task.sleep(nanoseconds: 100_000_000)
            domains = try await allDomains()
          }
        }
        if operation == "stop" {
          if let domain = domains.first(where: { $0.identifier.rawValue == id }) {
            try await remove(domain)
          }
          output(["removed": true])
          exit(0)
        }
        if operation == "start" {
          guard let port = command.port, let token = command.token, let pid = command.pid, pid > 1
          else { throw HostError.invalid }
          try FileBootstrap(port: port, token: token).validate()
          // Reap only our schema's domains whose owning process is definitely dead.
          // EPERM is treated as alive; never remove another running instance.
          for domain in domains
          where domain.identifier.rawValue.hasPrefix(prefix)
            && domain.userInfo?["schema"] as? String == "macrdp-files-v1"
          {
            if let owner = domain.userInfo?["pid"] as? NSNumber, owner.int32Value > 1,
              kill(owner.int32Value, 0) == -1, errno == ESRCH
            {
              try await remove(domain)
            }
          }
          let domain = NSFileProviderDomain(
            identifier: .init(id),
            displayName: "macrdp Clipboard " + String(command.instance.suffix(8)))
          domain.userInfo = [
            "schema": "macrdp-files-v1", "instance": command.instance,
            "port": NSNumber(value: port), "token": token, "pid": NSNumber(value: pid),
          ]
          try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<Void, Error>) in
            NSFileProviderManager.add(domain) { error in
              if let error { continuation.resume(throwing: error) } else { continuation.resume() }
            }
          }
          guard let manager = NSFileProviderManager(for: domain) else { throw HostError.missing }
          _ = try await manager.getUserVisibleURL(for: .rootContainer)
          output(["registered": true, "domain": id])
          exit(0)
        }
        guard let domain = domains.first(where: { $0.identifier.rawValue == id }) else {
          output([
            "error": "FileProviderDomainMissing", "requested": id,
            "available": domains.map { $0.identifier.rawValue },
          ])
          exit(1)
        }
        guard let manager = NSFileProviderManager(for: domain) else {
          output(["error": "FileProviderManagerUnavailable", "domain": id])
          exit(1)
        }
        if operation == "resolve" {
          for container in [NSFileProviderItemIdentifier.rootContainer, .workingSet] {
            try await withCheckedThrowingContinuation {
              (continuation: CheckedContinuation<Void, Error>) in
              manager.signalEnumerator(for: container) { error in
                if let error { continuation.resume(throwing: error) } else { continuation.resume() }
              }
            }
          }
          guard let items = command.items, !items.isEmpty, items.count <= 4096 else {
            throw HostError.invalid
          }
          var paths: [String] = []
          for item in items {
            paths.append(try await manager.getUserVisibleURL(for: .init(item)).path)
          }
          // Do not open/read the files here. This process is nonmaterializing.
          output(["paths": paths])
          exit(0)
        }
        throw HostError.usage
      } catch {
        let error = error as NSError
        output(["error": error.domain, "code": error.code, "message": error.localizedDescription])
        exit(1)
      }
    }
    application.run()

  }
}
