import Foundation

/// What the menu bar shows about the hub. Pure value; derivation from raw
/// HTTP results lives here so it is fully unit-testable.
public enum HubStatus: Equatable, Sendable {
    /// No probe has completed yet.
    case unknown
    /// Nothing answered on the port (hub not running / still booting).
    case down
    /// Hub answered `/api/health`.
    case up(version: String)

    /// Derive from a health probe outcome: response body on success, nil on
    /// any transport error. Unparseable bodies still count as "up" (something
    /// is serving the port) but without a version.
    public static func from(healthBody: Data?) -> HubStatus {
        guard let body = healthBody else { return .down }
        struct Health: Decodable { let status: String; let version: String? }
        guard let health = try? JSONDecoder().decode(Health.self, from: body) else {
            return .up(version: "")
        }
        return .up(version: health.version ?? "")
    }

    public var menuTitle: String {
        switch self {
        case .unknown: return "Checking hub…"
        case .down: return "Hub is not running"
        case .up(let v): return v.isEmpty ? "Hub is running" : "Hub is running — v\(v)"
        }
    }

    /// SF Symbol for the menu bar icon.
    public var symbolName: String {
        switch self {
        case .unknown: return "questionmark.circle"
        case .down: return "xmark.circle"
        case .up: return "checkmark.circle.fill"
        }
    }

    public var isUp: Bool {
        if case .up = self { return true }
        return false
    }
}
