import ActivityKit
import Foundation

/// Shared attributes for Herald Live Activities.
/// Used by both the main app (to start/update activities) and the widget extension (to render them).
struct HeraldActivityAttributes: ActivityAttributes, Sendable {
    /// Dynamic data — updated throughout the activity's lifetime.
    struct ContentState: Codable, Hashable, Sendable {
        // ── Session state (existing) ──────────────────────────────────
        var status: String            // "Listening", "Thinking", "Working on that…"
        var toolName: String?         // e.g., "herald_delegate", "vision_analyze"
        var elapsedSeconds: Int       // seconds since activity started
        var startDate: Date?          // used by Text(timerInterval:) for a live-ticking clock
        var sessionType: String       // "voice", "chat", "tool"
        var emoji: String?            // Contextual emoji for Dynamic Island

        // ── Gateway telemetry (Herald 2.3.0) ─────────────────────────
        var gatewayConnected: Bool = false
        var activeQueries: Int = 0
        var modelName: String?
        var version: String?
        var cpuPercent: Double = 0.0
        var memoryUsedGb: Double = 0.0
        var memoryTotalGb: Double = 0.0
        var uptimeHours: Double = 0.0
        var alertCount: Int = 0
    }

    /// Immutable for the lifetime of the activity.
    var agentName: String = "Herald"
}
