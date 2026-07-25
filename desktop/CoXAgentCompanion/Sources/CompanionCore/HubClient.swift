import Foundation

/// Outbound port: minimal HTTP GET. Injected so the client is testable
/// without a network.
public protocol HTTPTransport: Sendable {
    func get(_ url: URL) async throws -> Data
}

public struct URLSessionTransport: HTTPTransport {
    public init() {}

    public func get(_ url: URL) async throws -> Data {
        var request = URLRequest(url: url)
        request.timeoutInterval = 2
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode)
        else {
            throw URLError(.badServerResponse)
        }
        return data
    }
}

/// Use case: probe the hub and report its status. Never throws — transport
/// failures simply mean the hub is down.
public struct HubClient: Sendable {
    let config: CompanionConfig
    let transport: HTTPTransport

    public init(config: CompanionConfig, transport: HTTPTransport = URLSessionTransport()) {
        self.config = config
        self.transport = transport
    }

    public func probe() async -> HubStatus {
        let body = try? await transport.get(config.healthURL)
        return HubStatus.from(healthBody: body)
    }
}
