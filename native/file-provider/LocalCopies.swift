import Foundation

/// Local clipboard-cache edits never change the immutable Windows source identity.
struct LocalCopy: Codable {
  let revision: String
  let content: String?
  let size: Int64
  let flags: UInt
  let modified: Date?
  let lastUsed: Date?
  func validate() throws {
    guard UUID(uuidString: revision) != nil,
      content == nil || UUID(uuidString: content!) != nil,
      size >= 0, size <= 128 * 1024 * 1024 * 1024, flags & ~UInt(31) == 0
    else { throw FilesError.invalid }
  }
}

/// Copies bytes, not source permissions/immutable flags. Bounded memory; failed
/// or cancelled saves never replace the journal's previously committed version.
func stageLocalCopy(
  from source: URL, into directory: URL,
  maximumBytes: Int64 = 128 * 1024 * 1024 * 1024
) throws -> (String, Int64) {
  guard maximumBytes >= 0 else { throw FilesError.limit }
  let fm = FileManager.default
  try fm.createDirectory(
    at: directory, withIntermediateDirectories: true,
    attributes: [.posixPermissions: 0o700])
  let id = UUID().uuidString
  let path = directory.appendingPathComponent(id)
  guard
    fm.createFile(
      atPath: path.path, contents: nil,
      attributes: [.posixPermissions: 0o600])
  else { throw FilesError.unavailable }
  var committed = false
  defer { if !committed { try? fm.removeItem(at: path) } }
  let input = try FileHandle(forReadingFrom: source)
  defer { try? input.close() }
  let output = try FileHandle(forWritingTo: path)
  defer { try? output.close() }
  var size: Int64 = 0
  while true {
    try Task.checkCancellation()
    let bytes = try input.read(upToCount: 128 * 1024) ?? Data()
    if bytes.isEmpty { break }
    size += Int64(bytes.count)
    guard size <= maximumBytes else { throw FilesError.limit }
    try output.write(contentsOf: bytes)
  }
  try output.synchronize()
  try Task.checkCancellation()
  committed = true
  return (id, size)
}
