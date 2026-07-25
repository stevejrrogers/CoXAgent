import Foundation

/// Pure configuration for the companion. Port resolution matches the desktop
/// shell: `COXAGENT_DESKTOP_PORT` overrides, invalid values fall back to 4000.
public struct CompanionConfig: Equatable, Sendable {
    public static let defaultPort: UInt16 = 4000

    public let port: UInt16
    /// Workspace used by the shells: `~/CoXAgent`.
    public let workspace: URL

    public init(port: UInt16, workspace: URL) {
        self.port = port
        self.workspace = workspace
    }

    /// Resolve from an environment map + home directory (injected for tests).
    public static func resolve(environment: [String: String], home: URL) -> CompanionConfig {
        let port = environment["COXAGENT_DESKTOP_PORT"]
            .flatMap { UInt16($0.trimmingCharacters(in: .whitespaces)) }
            .flatMap { $0 == 0 ? nil : $0 }
            ?? defaultPort
        return CompanionConfig(port: port, workspace: home.appendingPathComponent("CoXAgent"))
    }

    public static func fromEnvironment() -> CompanionConfig {
        resolve(
            environment: ProcessInfo.processInfo.environment,
            home: FileManager.default.homeDirectoryForCurrentUser
        )
    }

    public var dashboardURL: URL { URL(string: "http://127.0.0.1:\(port)/")! }
    public var healthURL: URL { URL(string: "http://127.0.0.1:\(port)/api/health")! }
    public var hubLogURL: URL {
        workspace.appendingPathComponent("logs").appendingPathComponent("hub.log")
    }
}
