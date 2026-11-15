import Foundation

struct FileSnapshot: Codable, Equatable {
  let session: String
  let copy: String
  let lock: UInt32
  let index: Int32
  let name: String
  let size: Int64
  var identifier: String { "\(session)/\(copy)/\(index)" }
  var container: String { "batch/\(session)/\(copy)" }
  func validate() throws {
    let safeID: (String) -> Bool = {
      !$0.isEmpty && $0.utf8.count <= 128
        && $0.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") })
    }
    guard safeID(session), safeID(copy), index >= 0, lock > 0,
      !name.isEmpty, name != ".", name != "..", name.utf8.count <= 255,
      !name.contains("/"), !name.contains("\\"), !name.contains("\0"),
      size >= 0, size <= 128 * 1024 * 1024 * 1024
    else { throw FilesError.invalid }
  }
}
struct FileBootstrap: Codable {
  let port: UInt16
  let token: String
  func validate() throws {
    guard port > 0, token.utf8.count == 64, token.allSatisfy({ $0.isHexDigit }) else {
      throw FilesError.unauthorized
    }
  }
}
enum FilesError: Error { case unauthorized, stale, unavailable, invalid, limit }
struct Metadata: Decodable { let snapshots: [FileSnapshot] }
struct RangeReply: Decodable {
  let session: String
  let copy: String
  let lock: UInt32
  let index: Int32
  let offset: Int64
  let bytes: String
}
struct FilesClient {
  let bootstrap: FileBootstrap
  func request<T: Decodable>(_ payload: [String: Any], as: T.Type) async throws -> T {
    try bootstrap.validate()
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 30
    configuration.timeoutIntervalForResource = 35
    configuration.httpMaximumConnectionsPerHost = 2
    configuration.connectionProxyDictionary = [:]
    let session = URLSession(configuration: configuration)
    defer { session.invalidateAndCancel() }
    var request = URLRequest(url: URL(string: "http://127.0.0.1:\(bootstrap.port)/v1")!)
    request.httpMethod = "POST"
    request.httpBody = try JSONSerialization.data(withJSONObject: payload)
    request.setValue("Bearer " + bootstrap.token, forHTTPHeaderField: "Authorization")
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    let (bytes, response) = try await session.data(for: request)
    guard let http = response as? HTTPURLResponse else { throw FilesError.invalid }
    switch http.statusCode {
    case 200: break
    case 401: throw FilesError.unauthorized
    case 409: throw FilesError.stale
    default: throw FilesError.unavailable
    }
    guard bytes.count <= 2 * 1024 * 1024 else { throw FilesError.limit }
    return try JSONDecoder().decode(T.self, from: bytes)
  }
  func metadata() async throws -> [FileSnapshot] {
    let reply = try await request(["op": "metadata"], as: Metadata.self)
    guard reply.snapshots.count <= 4096 else { throw FilesError.limit }
    var ids = Set<String>()
    var names: [String: Set<String>] = [:]
    for file in reply.snapshots {
      try file.validate()
      guard ids.insert(file.identifier).inserted,
        names[file.container, default: []].insert(
          file.name.precomposedStringWithCanonicalMapping.lowercased()
        ).inserted
      else { throw FilesError.invalid }
    }
    return reply.snapshots
  }
  func range(_ file: FileSnapshot, offset: Int64, length: Int) async throws -> Data {
    guard offset >= 0, length > 0, length <= 128 * 1024, offset <= file.size - Int64(length) else {
      throw FilesError.invalid
    }
    let reply = try await request(
      [
        "op": "range", "session": file.session, "copy": file.copy, "lock": file.lock,
        "index": file.index, "offset": offset, "length": length,
      ], as: RangeReply.self)
    guard reply.session == file.session, reply.copy == file.copy, reply.lock == file.lock,
      reply.index == file.index, reply.offset == offset,
      let bytes = Data(base64Encoded: reply.bytes), !bytes.isEmpty, bytes.count <= length
    else { throw FilesError.stale }
    return bytes
  }
  /// Stream into an unpublished private file; only a complete file leaves this boundary.
  func materialize(_ file: FileSnapshot, directory: URL) async throws -> URL {
    try file.validate()
    try Task.checkCancellation()
    let partial = directory.appendingPathComponent(UUID().uuidString + ".partial")
    let complete = directory.appendingPathComponent(UUID().uuidString + ".ready")
    guard
      FileManager.default.createFile(
        atPath: partial.path, contents: nil, attributes: [.posixPermissions: 0o600])
    else { throw FilesError.unavailable }
    defer { try? FileManager.default.removeItem(at: partial) }
    let handle = try FileHandle(forWritingTo: partial)
    defer { try? handle.close() }
    var offset: Int64 = 0
    while offset < file.size {
      try Task.checkCancellation()
      let data = try await range(
        file, offset: offset, length: Int(min(128 * 1024, file.size - offset)))
      try handle.write(contentsOf: data)
      offset += Int64(data.count)
    }
    try Task.checkCancellation()
    try handle.synchronize()
    try handle.close()
    try FileManager.default.moveItem(at: partial, to: complete)
    return complete
  }
}
private struct CatalogArchive: Codable {
  let generation: UInt64
  let files: [String: FileSnapshot]
  let inserted: [String: UInt64]
}
actor FilesCatalog {
  let client: FilesClient
  let archive: URL
  var files: [String: FileSnapshot]
  var inserted: [String: UInt64]
  var generation: UInt64
  init(client: FilesClient, archive: URL) throws {
    self.client = client
    self.archive = archive
    if FileManager.default.fileExists(atPath: archive.path) {
      let size = try FileManager.default.attributesOfItem(atPath: archive.path)[.size] as? NSNumber
      guard let size, size.intValue <= 4 * 1024 * 1024 else { throw FilesError.limit }
      let data = try Data(contentsOf: archive)
      let saved = try JSONDecoder().decode(CatalogArchive.self, from: data)
      files = saved.files
      inserted = saved.inserted
      generation = saved.generation
      guard files.count <= 4096, Set(inserted.keys) == Set(files.keys) else {
        throw FilesError.limit
      }
      for (key, file) in files {
        try file.validate()
        guard key == file.identifier, let value = inserted[key], value > 0, value <= generation
        else { throw FilesError.invalid }
      }
    } else {
      files = [:]
      inserted = [:]
      generation = 0
    }
  }
  func refresh() async throws {
    let fresh = try await client.metadata()
    var next = files
    var added: [String] = []
    for file in fresh {
      if let old = next[file.identifier], old != file { throw FilesError.stale }
      if next[file.identifier] == nil { added.append(file.identifier) }
      next[file.identifier] = file
    }
    guard next.count <= 4096 else { throw FilesError.limit }
    guard !added.isEmpty else { return }
    guard generation < UInt64.max else { throw FilesError.limit }
    let revision = generation + 1
    var positions = inserted
    for key in added { positions[key] = revision }
    let data = try JSONEncoder().encode(
      CatalogArchive(generation: revision, files: next, inserted: positions))
    guard data.count <= 4 * 1024 * 1024 else { throw FilesError.limit }
    try data.write(to: archive, options: .atomic)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: archive.path)
    files = next
    inserted = positions
    generation = revision
  }
  func snapshot(_ id: String) async throws -> FileSnapshot {
    if files[id] == nil { try await refresh() }
    guard let file = files[id] else { throw FilesError.stale }
    return file
  }
  func children(_ container: String) async throws -> [FileSnapshot] {
    do { try await refresh() } catch { if files.isEmpty { throw error } }
    return files.values.filter { container == "root" || $0.container == container }.sorted {
      $0.identifier < $1.identifier
    }
  }
  func changes(since anchor: UInt64, container: String) async throws -> (
    files: [FileSnapshot], batches: [String], anchor: UInt64
  ) {
    do { try await refresh() } catch { if files.isEmpty { throw error } }
    guard anchor <= generation else { throw FilesError.stale }
    let selected = files.values.filter {
      inserted[$0.identifier, default: 0] > anchor
        && (container == "root" || container == "working" || $0.container == container)
    }
    let batches = Set(selected.map { $0.container }).filter { batch in
      files.values.filter { $0.container == batch }.map { inserted[$0.identifier, default: 0] }
        .min()! > anchor
    }
    return (Array(selected).sorted { $0.identifier < $1.identifier }, batches.sorted(), generation)
  }
  func anchor() -> UInt64 { generation }
}
