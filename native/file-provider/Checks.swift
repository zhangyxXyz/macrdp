import CryptoKit
import Foundation

private struct Input: Decodable {
  let port: UInt16
  let token: String
  let operation: String
  let directory: String
  var index: Int32?
}
@main
struct Checks {
  static func main() async {
    do {
      let input = try JSONDecoder().decode(
        Input.self, from: FileHandle.standardInput.readDataToEndOfFile())
      let client = FilesClient(bootstrap: FileBootstrap(port: input.port, token: input.token))
      let directory = URL(fileURLWithPath: input.directory)
      try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
      var result: [String: Any] = [:]
      if input.operation == "model" {
        let valid = FileSnapshot(
          session: "session-A", copy: "copy-A", lock: 7, index: 0, name: "same.bin", size: 0)
        try valid.validate()
        for name in ["", ".", "..", "../file", "a/b", "a\\b", "a\0b"] {
          do {
            try FileSnapshot(session: "A", copy: "B", lock: 7, index: 0, name: name, size: 1)
              .validate()
            fatalError("unsafe name accepted")
          } catch {}
        }
        result = ["ok": true]
      } else if input.operation == "catalog" {
        let archive = directory.appendingPathComponent("metadata.json")
        let first = try FilesCatalog(client: client, archive: archive)
        try await first.refresh()
        let files = try await first.children("root")
        guard let file = files.first else { throw FilesError.invalid }
        let restored = try FilesCatalog(client: client, archive: archive)
        guard try await restored.snapshot(file.identifier) == file else { throw FilesError.invalid }
        let anchor = await restored.anchor()
        let changes = try await restored.changes(since: anchor, container: "root")
        guard changes.files.isEmpty, changes.batches.isEmpty, changes.anchor == anchor else {
          throw FilesError.invalid
        }
        result = ["ok": true, "count": files.count]
      } else {
        let files = try await client.metadata()
        if input.operation == "metadata" {
          result = ["ok": true, "count": files.count]
        } else {
          guard let file = files.first(where: { $0.index == (input.index ?? 0) }) else {
            throw FilesError.invalid
          }
          let task = Task { try await client.materialize(file, directory: directory) }
          if input.operation == "cancel" {
            try await Task.sleep(nanoseconds: 50_000_000)
            task.cancel()
          }
          let path = try await task.value
          let bytes = try Data(contentsOf: path, options: .mappedIfSafe)
          result = [
            "ok": true, "size": bytes.count, "path": path.path,
            "sha256": SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined(),
          ]
        }
      }
      let bytes = try JSONSerialization.data(withJSONObject: result, options: .sortedKeys)
      FileHandle.standardOutput.write(bytes + Data([10]))
    } catch {
      let value: [String: Any] = ["ok": false, "error": String(describing: error)]
      let data = try! JSONSerialization.data(withJSONObject: value, options: .sortedKeys)
      FileHandle.standardOutput.write(data + Data([10]))
    }
  }
}
