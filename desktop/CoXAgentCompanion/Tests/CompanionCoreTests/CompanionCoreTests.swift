import Foundation
import Testing

@testable import CompanionCore

private let home = URL(fileURLWithPath: "/Users/u")

@Suite struct ConfigTests {
    @Test func defaults() {
        let cfg = CompanionConfig.resolve(environment: [:], home: home)
        #expect(cfg.port == 4000)
        #expect(cfg.dashboardURL.absoluteString == "http://127.0.0.1:4000/")
        #expect(cfg.healthURL.absoluteString == "http://127.0.0.1:4000/api/health")
        #expect(cfg.hubLogURL.path == "/Users/u/CoXAgent/logs/hub.log")
    }

    @Test func portOverride() {
        let cfg = CompanionConfig.resolve(
            environment: ["COXAGENT_DESKTOP_PORT": " 8123 "], home: home)
        #expect(cfg.port == 8123)
        #expect(cfg.dashboardURL.absoluteString == "http://127.0.0.1:8123/")
    }

    @Test(arguments: ["abc", "0", "70000", ""])
    func invalidPortsFallBack(bad: String) {
        let cfg = CompanionConfig.resolve(
            environment: ["COXAGENT_DESKTOP_PORT": bad], home: home)
        #expect(cfg.port == 4000)
    }
}

@Suite struct HubStatusTests {
    @Test func downWhenNoBody() {
        #expect(HubStatus.from(healthBody: nil) == .down)
    }

    @Test func upWithVersion() {
        let body = Data(#"{"status":"ok","version":"2.8.1"}"#.utf8)
        #expect(HubStatus.from(healthBody: body) == .up(version: "2.8.1"))
    }

    @Test func upWithoutVersionField() {
        let body = Data(#"{"status":"ok"}"#.utf8)
        #expect(HubStatus.from(healthBody: body) == .up(version: ""))
    }

    @Test func unparseableBodyStillCountsAsUp() {
        #expect(HubStatus.from(healthBody: Data("<html>".utf8)) == .up(version: ""))
    }

    @Test func menuPresentation() {
        #expect(HubStatus.up(version: "2.8.1").menuTitle == "Hub is running — v2.8.1")
        #expect(HubStatus.up(version: "").menuTitle == "Hub is running")
        #expect(HubStatus.down.menuTitle == "Hub is not running")
        #expect(HubStatus.unknown.symbolName == "questionmark.circle")
        #expect(HubStatus.down.symbolName == "xmark.circle")
        #expect(HubStatus.up(version: "1").symbolName == "checkmark.circle.fill")
        #expect(HubStatus.up(version: "1").isUp)
        #expect(!HubStatus.down.isUp)
    }
}

/// Transport stub: canned response or error, records requested URLs.
private struct StubTransport: HTTPTransport {
    let result: Result<Data, any Error>
    let seen: @Sendable (URL) -> Void

    func get(_ url: URL) async throws -> Data {
        seen(url)
        return try result.get()
    }
}

/// Tiny thread-safe box so the Sendable stub can record what it saw.
private final class LockedBox<T>: @unchecked Sendable {
    private var value: T
    private let lock = NSLock()
    init(_ value: T) { self.value = value }
    func set(_ new: T) {
        lock.lock()
        value = new
        lock.unlock()
    }
    func get() -> T {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

@Suite struct HubClientTests {
    let cfg = CompanionConfig.resolve(environment: [:], home: URL(fileURLWithPath: "/u"))

    @Test func probeUpHitsHealthEndpoint() async {
        let requested = LockedBox<URL?>(nil)
        let client = HubClient(
            config: cfg,
            transport: StubTransport(
                result: .success(Data(#"{"status":"ok","version":"9.9"}"#.utf8)),
                seen: { requested.set($0) }
            ))
        let status = await client.probe()
        #expect(status == .up(version: "9.9"))
        #expect(requested.get()?.absoluteString == "http://127.0.0.1:4000/api/health")
    }

    @Test func probeDownOnTransportError() async {
        let client = HubClient(
            config: cfg,
            transport: StubTransport(
                result: .failure(URLError(.cannotConnectToHost)), seen: { _ in })
        )
        let status = await client.probe()
        #expect(status == .down)
    }
}
