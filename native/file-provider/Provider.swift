import FileProvider
import Foundation
import OSLog
import UniformTypeIdentifiers

private func providerError(_ error: Error) -> NSError {
  if error is CancellationError {
    return NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError)
  }
  var code = NSFileProviderError.serverUnreachable
  if let error = error as? FilesError {
    switch error {
    case .stale: code = .versionNoLongerAvailable
    case .unauthorized: code = .notAuthenticated
    default: break
    }
  }
  return NSError(domain: NSFileProviderErrorDomain, code: code.rawValue)
}
private final class FileItem: NSObject, NSFileProviderItem {
  let file: FileSnapshot?
  let batch: String?
  init(file: FileSnapshot? = nil, batch: String? = nil) {
    self.file = file
    self.batch = batch
  }
  var itemIdentifier: NSFileProviderItemIdentifier {
    .init(file?.identifier ?? batch ?? NSFileProviderItemIdentifier.rootContainer.rawValue)
  }
  var parentItemIdentifier: NSFileProviderItemIdentifier {
    file.map { .init($0.container) } ?? .rootContainer
  }
  var filename: String {
    file?.name ?? batch.flatMap { $0.split(separator: "/").last }.map(String.init)
      ?? "macrdp Clipboard"
  }
  var contentType: UTType {
    file.map { UTType(filenameExtension: ($0.name as NSString).pathExtension) ?? .data } ?? .folder
  }
  var capabilities: NSFileProviderItemCapabilities {
    file == nil ? [.allowsReading, .allowsContentEnumerating] : [.allowsReading]
  }
  var documentSize: NSNumber? { file.map { NSNumber(value: $0.size) } }
  var itemVersion: NSFileProviderItemVersion {
    .init(
      contentVersion: Data(itemIdentifier.rawValue.utf8),
      metadataVersion: Data(itemIdentifier.rawValue.utf8))
  }
  var contentPolicy: NSFileProviderContentPolicy { .downloadLazily }
}
private final class WorkRegistry {
  let lock = NSLock()
  var tasks: [UUID: Task<Void, Never>] = [:]
  var invalidated = false
  func start(_ operation: @escaping () async -> Void) -> Task<Void, Never> {
    let id = UUID()
    lock.lock()
    let task = Task {
      await operation()
      self.finish(id)
    }
    tasks[id] = task
    if invalidated { task.cancel() }
    lock.unlock()
    return task
  }
  private func finish(_ id: UUID) {
    lock.lock()
    tasks.removeValue(forKey: id)
    lock.unlock()
  }
  func cancelAll() {
    lock.lock()
    invalidated = true
    let values = Array(tasks.values)
    tasks.removeAll()
    lock.unlock()
    for task in values { task.cancel() }
  }
}

private final class FilesEnumerator: NSObject, NSFileProviderEnumerator {
  let catalog: FilesCatalog
  let container: NSFileProviderItemIdentifier
  private let work = WorkRegistry()
  init(_ catalog: FilesCatalog, _ container: NSFileProviderItemIdentifier) {
    self.catalog = catalog
    self.container = container
  }
  func invalidate() { work.cancelAll() }
  func enumerateItems(
    for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage
  ) {
    _ = work.start { [self] in
      do {
        var items: [FileItem] = []
        if container == .rootContainer {
          let files = try await catalog.children("root")
          items = Array(Set(files.map { $0.container })).sorted().map { FileItem(batch: $0) }
        } else if container == .workingSet {
          let files = try await catalog.children("root")
          items =
            Array(Set(files.map { $0.container })).sorted().map { FileItem(batch: $0) }
            + files.map { FileItem(file: $0) }
        } else if container != .trashContainer {
          items = try await catalog.children(container.rawValue).map { FileItem(file: $0) }
        }
        try Task.checkCancellation()
        observer.didEnumerate(items)
        observer.finishEnumerating(upTo: nil)
      } catch { observer.finishEnumeratingWithError(providerError(error)) }
    }
  }
  func enumerateChanges(
    for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor
  ) {
    _ = work.start { [self] in
      do {
        guard anchor.rawValue.count <= 32,
          let text = String(data: anchor.rawValue, encoding: .utf8), let value = UInt64(text)
        else { throw FilesError.stale }
        let scope =
          container == .rootContainer
          ? "root" : container == .workingSet ? "working" : container.rawValue
        let changes = try await catalog.changes(since: value, container: scope)
        var items: [FileItem] = []
        if container == .rootContainer || container == .workingSet {
          items += changes.batches.map { FileItem(batch: $0) }
        }
        if container != .rootContainer && container != .trashContainer {
          items += changes.files.map { FileItem(file: $0) }
        }
        try Task.checkCancellation()
        observer.didUpdate(items)
        observer.finishEnumeratingChanges(
          upTo: NSFileProviderSyncAnchor(Data(String(changes.anchor).utf8)), moreComing: false)
      } catch {
        if let value = error as? FilesError, case .stale = value {
          observer.finishEnumeratingWithError(
            NSError(
              domain: NSFileProviderErrorDomain,
              code: NSFileProviderError.syncAnchorExpired.rawValue))
        } else {
          observer.finishEnumeratingWithError(providerError(error))
        }
      }
    }
  }
  func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
    _ = work.start { [self] in
      completionHandler(NSFileProviderSyncAnchor(Data(String(await catalog.anchor()).utf8)))
    }
  }

}
@objc(MacrdpFilesExtension)
final class MacrdpFilesExtension: NSObject, NSFileProviderReplicatedExtension {
  let manager: NSFileProviderManager?
  let catalog: FilesCatalog?
  private let work = WorkRegistry()
  let log = Logger(subsystem: "com.clintcan.macrdp.files", category: "materialization")
  required init(domain: NSFileProviderDomain) {
    manager = NSFileProviderManager(for: domain)
    if domain.userInfo?["schema"] as? String == "macrdp-files-v1",
      let port = domain.userInfo?["port"] as? NSNumber, let value = UInt16(exactly: port.intValue),
      let token = domain.userInfo?["token"] as? String,
      let instance = domain.userInfo?["instance"] as? String, UUID(uuidString: instance) != nil
    {
      let bootstrap = FileBootstrap(port: value, token: token)
      do {
        try bootstrap.validate()
        let root = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
          .first!.appendingPathComponent("Clipboard-" + instance, isDirectory: true)
        try FileManager.default.createDirectory(
          at: root, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        catalog = try FilesCatalog(
          client: FilesClient(bootstrap: bootstrap),
          archive: root.appendingPathComponent("metadata.json"))
      } catch { catalog = nil }
    } else {
      catalog = nil
    }
    super.init()
  }
  func invalidate() { work.cancelAll() }
  func enumerator(for container: NSFileProviderItemIdentifier, request: NSFileProviderRequest)
    throws -> NSFileProviderEnumerator
  {
    guard let catalog else { throw providerError(FilesError.unauthorized) }
    guard
      [.rootContainer, .workingSet, .trashContainer].contains(container)
        || container.rawValue.hasPrefix("batch/")
    else { throw providerError(FilesError.stale) }
    return FilesEnumerator(catalog, container)
  }
  func item(
    for identifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest,
    completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
  ) -> Progress {
    let progress = Progress(totalUnitCount: 1)
    let task = work.start { [self] in
      do {
        guard let catalog else { throw FilesError.unauthorized }
        let item: FileItem
        if identifier == .rootContainer {
          item = FileItem()
        } else if identifier.rawValue.hasPrefix("batch/") {
          guard try await !catalog.children(identifier.rawValue).isEmpty else {
            throw FilesError.stale
          }
          item = FileItem(batch: identifier.rawValue)
        } else {
          item = try await FileItem(file: catalog.snapshot(identifier.rawValue))
        }
        try Task.checkCancellation()
        progress.completedUnitCount = 1
        completionHandler(item, nil)
      } catch { completionHandler(nil, providerError(error)) }
    }
    progress.cancellationHandler = { task.cancel() }
    return progress
  }
  func fetchContents(
    for identifier: NSFileProviderItemIdentifier, version: NSFileProviderItemVersion?,
    request: NSFileProviderRequest,
    completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
  ) -> Progress {
    let progress = Progress(totalUnitCount: 1)
    let task = work.start { [self] in
      do {
        guard let catalog, let manager else { throw FilesError.unauthorized }
        let snapshot = try await catalog.snapshot(identifier.rawValue)
        let item = FileItem(file: snapshot)
        if let version, version != item.itemVersion { throw FilesError.stale }
        self.log.info("fetch start index=\(snapshot.index) bytes=\(snapshot.size)")
        let path = try await catalog.client.materialize(
          snapshot, directory: manager.temporaryDirectoryURL())
        do { try Task.checkCancellation() } catch {
          try? FileManager.default.removeItem(at: path)
          throw error
        }
        progress.completedUnitCount = 1
        self.log.info("fetch complete index=\(snapshot.index)")
        completionHandler(path, item, nil)
      } catch { completionHandler(nil, nil, providerError(error)) }
    }
    progress.cancellationHandler = { task.cancel() }
    return progress
  }
  func createItem(
    basedOn itemTemplate: NSFileProviderItem, fields: NSFileProviderItemFields, contents: URL?,
    options: NSFileProviderCreateItemOptions, request: NSFileProviderRequest,
    completionHandler:
      @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
  ) -> Progress {
    completionHandler(
      nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFileWriteNoPermissionError))
    return Progress()
  }
  func modifyItem(
    _ item: NSFileProviderItem, baseVersion: NSFileProviderItemVersion,
    changedFields: NSFileProviderItemFields, contents: URL?,
    options: NSFileProviderModifyItemOptions, request: NSFileProviderRequest,
    completionHandler:
      @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
  ) -> Progress {
    completionHandler(
      nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFileWriteNoPermissionError))
    return Progress()
  }
  func deleteItem(
    identifier: NSFileProviderItemIdentifier, baseVersion: NSFileProviderItemVersion,
    options: NSFileProviderDeleteItemOptions, request: NSFileProviderRequest,
    completionHandler: @escaping (Error?) -> Void
  ) -> Progress {
    completionHandler(NSError(domain: NSCocoaErrorDomain, code: NSFileWriteNoPermissionError))
    return Progress()
  }
}
