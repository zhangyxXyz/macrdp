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
      } else if input.operation == "local-copy" {
        let archive = directory.appendingPathComponent("metadata.json")
        let catalog = try FilesCatalog(client: client, archive: archive)
        try await catalog.refresh()
        let file = try await catalog.children("root")[0]
        let anchor = await catalog.anchor()
        let source = directory.appendingPathComponent("edited.bin")
        let bytes = Data("local edited bytes".utf8)
        try bytes.write(to: source)
        let version = Data(file.identifier.utf8)
        let saved = try await catalog.saveLocalCopy(
          file.identifier, contentVersion: version,
          metadataVersion: version, contents: source, flags: nil, modified: nil, lastUsed: nil)
        guard saved.size == bytes.count, saved.content != nil else { throw FilesError.invalid }
        do {
          _ = try await catalog.saveLocalCopy(
            file.identifier, contentVersion: version,
            metadataVersion: version, contents: source, flags: nil, modified: nil, lastUsed: nil)
          fatalError("stale edit accepted")
        } catch FilesError.stale {}
        let localDirectory = directory.appendingPathComponent("local-copies")
        let orphan = localDirectory.appendingPathComponent(UUID().uuidString)
        try Data("aborted save".utf8).write(to: orphan)
        let count = try FileManager.default.contentsOfDirectory(atPath: localDirectory.path).count
        do {
          _ = try stageLocalCopy(from: source, into: localDirectory, maximumBytes: 1)
          fatalError("byte budget exceeded")
        } catch FilesError.limit {}
        guard
          try FileManager.default.contentsOfDirectory(atPath: localDirectory.path).count == count
        else { throw FilesError.invalid }
        let restored = try FilesCatalog(client: client, archive: archive)
        guard !FileManager.default.fileExists(atPath: orphan.path),
          try await restored.snapshot(file.identifier) == file
        else { throw FilesError.invalid }
        let path = try await restored.materializeLocalCopy(
          file.identifier,
          version: Data(saved.content!.utf8), directory: directory)!
        guard try Data(contentsOf: path) == bytes else { throw FilesError.invalid }
        let changes = try await restored.changes(since: anchor, container: "working")
        guard changes.files == [file], changes.anchor > anchor else { throw FilesError.invalid }
        // Unknown version and invalid metadata cannot replace the committed data.
        do {
          _ = try await restored.materializeLocalCopy(
            file.identifier, version: version, directory: directory)
          fatalError("stale read accepted")
        } catch FilesError.stale {}
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
