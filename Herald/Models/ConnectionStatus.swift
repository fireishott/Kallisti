import SwiftUI

enum ConnectionStatus: String, Codable, Hashable, Sendable {
    case disconnected      // Not connected
    case connecting        // Trying to connect
    case connected         // Fully operational
    case reconnecting      // Was connected, now re-establishing
    case degraded          // Connected but some services unavailable
    case error             // Legacy: connection error (maps to disconnected)

    var displayLabel: String {
        switch self {
        case .disconnected: "Disconnected"
        case .connecting: "Connecting..."
        case .connected: "Connected"
        case .reconnecting: "Reconnecting..."
        case .degraded: "Degraded"
        case .error: "Error"
        }
    }

    var displayIcon: String {
        switch self {
        case .disconnected: "xmark.circle.fill"
        case .connecting: "arrow.triangle.2.circlepath"
        case .connected: "checkmark.circle.fill"
        case .reconnecting: "arrow.triangle.2.circlepath"
        case .degraded: "exclamationmark.triangle.fill"
        case .error: "exclamationmark.circle.fill"
        }
    }

    var displayColor: Color {
        switch self {
        case .disconnected: .secondary
        case .connecting: .orange
        case .connected: .green
        case .reconnecting: .yellow
        case .degraded: .orange
        case .error: .red
        }
    }

    /// Dot color for the compact status indicator in the toolbar.
    var dotColor: Color {
        switch self {
        case .connected: .green
        case .connecting, .reconnecting: .yellow
        case .degraded: .orange
        case .disconnected, .error: .gray
        }
    }
}
