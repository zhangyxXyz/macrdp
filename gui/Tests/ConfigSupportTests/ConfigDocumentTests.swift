import Foundation
import ConfigSupport

final class ConfigDocumentTests {
    var cleanup: [() -> Void] = []
    deinit { cleanup.forEach { $0() } }
    func addTeardownBlock(_ block: @escaping () -> Void) { cleanup.append(block) }
    func testServerCompatibleParsingAndChangedKeyDeduplication() throws {
        let source = "# keep\r\nexport\t VD_HIDPI='1'\r\nVD_HIDPI=\"auto\"\r\nUNKNOWN='keep me'\r\nEXTRA_FLAGS=--password secret --custom\r\n"
        let doc = try ConfigDocument(data: Data(source.utf8))
        checkEqual(doc.values["VD_HIDPI"], "auto")
        let changed = try doc.replacing(["VD_HIDPI": "0", "VD_WIDTH": "3840"])
        let roundtrip = try ConfigDocument(data: changed)
        checkEqual(roundtrip.values["VD_HIDPI"], "0")
        checkEqual(roundtrip.values["UNKNOWN"], "keep me")
        checkEqual(roundtrip.values["EXTRA_FLAGS"], "--password secret --custom")
        let text = String(decoding: changed, as: UTF8.self)
        check(text.contains("# keep\r\n"))
        check(text.contains("UNKNOWN='keep me'"))
        checkEqual(text.components(separatedBy: "VD_HIDPI=").count, 2)
        check(text.hasSuffix("\n"))
    }
    func testRejectsMultilineChanges() throws {
        let doc = try ConfigDocument(data: Data())
        checkThrows(try doc.replacing(["VD_WIDTH": "3840\nBIND=bad"]))
    }
    private func fixture(_ body: String = "VD_WIDTH=1920\n") throws -> (URL, Data) {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        let url = dir.appendingPathComponent("config.env")
        let data = Data(body.utf8)
        try data.write(to: url)
        return (url, data)
    }
    func testRejectedCandidateLeavesLiveFileAndServiceUntouched() throws {
        let (url, old) = try fixture()
        var activated = false
        checkThrows(try ConfigTransaction.apply(url: url, expected: old, changes: ["VD_WIDTH":"0"], validate: { _ in throw ConfigError.invalid("bad") }, activate: { activated = true }))
        checkEqual(try Data(contentsOf: url), old)
        checkFalse(activated)
    }
    func testExternalChangeDuringValidationIsNotOverwritten() throws {
        let (url, old) = try fixture()
        let external = Data("VD_WIDTH=2560\n".utf8)
        checkThrows(try ConfigTransaction.apply(url: url, expected: old, changes: ["VD_WIDTH":"3840"], validate: { _ in try external.write(to: url) }, activate: { fail("must not restart") }))
        checkEqual(try Data(contentsOf: url), external)
    }
    func testActivationFailureRestoresExactOriginalBytes() throws {
        let (url, old) = try fixture("# preserved\nVD_WIDTH='1920'")
        var activations = 0
        checkThrows(try ConfigTransaction.apply(url: url, expected: old, changes: ["VD_WIDTH":"3840"], validate: { _ in }, activate: {
            activations += 1
            if activations == 1 { throw ConfigError.invalid("startup failed") }
            checkEqual(try Data(contentsOf: url), old)
        }))
        checkEqual(activations, 2)
        checkEqual(try Data(contentsOf: url), old)
    }
    func testSuccessfulTransactionWritesAllKeysAndActivatesOnce() throws {
        let (url, old) = try fixture()
        var count = 0
        try ConfigTransaction.apply(url: url, expected: old, changes: ["VD_WIDTH":"3840", "VD_HEIGHT":"2160"], validate: { candidate in
            checkEqual(try Data(contentsOf: url), old)
            checkEqual(try ConfigDocument(data: Data(contentsOf: candidate)).values["VD_HEIGHT"], "2160")
        }, activate: { count += 1 })
        checkEqual(count, 1)
        checkEqual(try Data(contentsOf: url.appendingPathExtension("previous")), old)
    }
    func testUneditedH264AckLimitSurvivesConfigurationApply() throws {
        let line = "H264_MAX_UNACKED_FRAMES='4'"
        let (url, old) = try fixture("# Advanced video setting, not managed by the GUI\n\(line)\nENABLE_H264=1\n")
        try ConfigTransaction.apply(url: url, expected: old, changes: ["MAP_CTRL_TO_CMD":"0"], validate: { candidate in
            let document = try ConfigDocument(data: Data(contentsOf: candidate))
            checkEqual(document.values["H264_MAX_UNACKED_FRAMES"], "4")
        }, activate: {})
        let saved = try Data(contentsOf: url)
        check(String(decoding: saved, as: UTF8.self).contains(line + "\n"))
        checkEqual(try ConfigDocument(data: saved).values["MAP_CTRL_TO_CMD"], "0")
    }
    func testRestartCommandAllowsSlowCleanupAndInspectionStillTimesOut() throws {
        // Normal dual-display restoration measured 10.5s. The old generic 8s
        // command limit would falsely roll back this successful restart.
        let (code, output) = try ConfigurationCommand.run("/bin/sleep", ["10.5"], timeout: ServiceTiming.restartCommandTimeout)
        checkEqual(code, 0)
        check(output.isEmpty)
        checkThrows(try ConfigurationCommand.run("/bin/sleep", ["2"], timeout: 0.05))
    }
    func testConfigurationCommandKeepsBoundedTailWithoutPipeDeadlock() throws {
        let (code, output) = try ConfigurationCommand.run("/usr/bin/head", ["-c", "131072", "/dev/zero"])
        checkEqual(code, 0)
        checkEqual(output.count, 65536)
        check(output.allSatisfy { $0 == 0 })
    }
    func testPrivatePermissionsAndRollbackExternalChange() throws {
        let (url, old) = try fixture()
        try FileManager.default.setAttributes([.posixPermissions: 0o640], ofItemAtPath: url.path)
        let external = Data("VD_WIDTH=1600\n".utf8)
        var activations = 0
        checkThrows(try ConfigTransaction.apply(url: url, expected: old, changes: ["VD_WIDTH":"3840"], validate: { candidate in
            let permissions = try FileManager.default.attributesOfItem(atPath: candidate.path)[.posixPermissions] as! NSNumber
            checkEqual(permissions.intValue, 0o600)
        }, activate: {
            activations += 1
            try external.write(to: url)
            throw ConfigError.invalid("external writer changed the config")
        }))
        checkEqual(activations, 1)
        checkEqual(try Data(contentsOf: url), external)
        let permissions = try FileManager.default.attributesOfItem(atPath: url.appendingPathExtension("previous").path)[.posixPermissions] as! NSNumber
        checkEqual(permissions.intValue, 0o600)
    }
    func testProtocolVersionsAndUnknownDisplayFields() throws {
        let capabilities = try ServerCapabilities(data: Data("{\"schema_version\":1,\"features\":[\"config_validation\"],\"supported_config_keys\":[\"RESOLUTION_POLICY\",\"LIVE_RESIZE\",\"VD_HIDPI\"]}".utf8))
        check(capabilities.canConfigureDisplay)
        checkThrows(try ServerCapabilities(data: Data("{\"schema_version\":2}".utf8)))
        checkThrows(try ConfigValidation(data: Data("{\"schema_version\":1,\"ok\":false,\"errors\":[\"conflict\"]}".utf8)))
        let display = try DisplaySnapshot(data: Data("{\"schema_version\":2,\"display\":{\"phase\":\"idle\",\"actual\":{\"backing_width\":null,\"scale_percent\":null}}}".utf8))
        checkEqual(display.dimensions("actual", width: "backing_width", height: "backing_height"), "Unknown")
        checkEqual(display.scale("actual"), "Unknown")
        checkEqual(display.pid, nil)
    }
    func testConcurrentControllerCannotReplaceCandidate() throws {
        let (url, old) = try fixture()
        try ConfigTransaction.apply(url: url, expected: old, changes: ["VD_WIDTH":"3840"], validate: { _ in
            checkThrows(try ConfigTransaction.apply(url: url, expected: old, changes: ["VD_WIDTH":"1600"], validate: { _ in }, activate: { fail("second controller must not restart") }))
        }, activate: {})
        checkEqual(try ConfigDocument(data: Data(contentsOf: url)).values["VD_WIDTH"], "3840")
    }
    func testExtraFlagsGroupingMatchesServerGrammar() throws {
        let flags = try ExtraFlags("'--no-client-resolution' \"--resolution-policy=fixed\" --virtual-display-hidpi 'auto' --label hello\\ world --empty ''")
        check(flags.contains("--no-client-resolution"))
        checkEqual(flags.value("--resolution-policy"), "fixed")
        checkEqual(flags.value("--virtual-display-hidpi"), "auto")
        checkEqual(flags.value("--label"), "hello world")
        checkEqual(flags.tokens.last, "")
        checkThrows(try ExtraFlags("--width '3840"))
        checkThrows(try ExtraFlags("--width 3840\\"))
    }
}

private func checkEqual<T: Equatable>(_ lhs: T, _ rhs: T, file: StaticString = #file, line: UInt = #line) { if lhs != rhs { fatalError("Expected \(rhs), got \(lhs)", file: file, line: line) } }
private func check(_ condition: Bool, file: StaticString = #file, line: UInt = #line) { if !condition { fatalError("Assertion failed", file: file, line: line) } }
private func checkFalse(_ condition: Bool, file: StaticString = #file, line: UInt = #line) { check(!condition, file: file, line: line) }
private func fail(_ message: String) { fatalError(message) }
private func checkThrows<T>(_ expression: @autoclosure () throws -> T, file: StaticString = #file, line: UInt = #line) { do { _ = try expression() } catch { return }; fatalError("Expected an error", file: file, line: line) }
@main struct ConfigSupportTestRunner {
    static func main() throws {
        let tests = ConfigDocumentTests()
        try tests.testServerCompatibleParsingAndChangedKeyDeduplication()
        try tests.testRejectsMultilineChanges()
        try tests.testRejectedCandidateLeavesLiveFileAndServiceUntouched()
        try tests.testExternalChangeDuringValidationIsNotOverwritten()
        try tests.testActivationFailureRestoresExactOriginalBytes()
        try tests.testSuccessfulTransactionWritesAllKeysAndActivatesOnce()
        try tests.testUneditedH264AckLimitSurvivesConfigurationApply()
        try tests.testRestartCommandAllowsSlowCleanupAndInspectionStillTimesOut()
        try tests.testConfigurationCommandKeepsBoundedTailWithoutPipeDeadlock()
        try tests.testPrivatePermissionsAndRollbackExternalChange()
        try tests.testProtocolVersionsAndUnknownDisplayFields()
        try tests.testConcurrentControllerCannotReplaceCandidate()
        try tests.testExtraFlagsGroupingMatchesServerGrammar()
        print("ConfigSupport: 13 tests passed (no server or user configuration touched)")
    }
}
