import ActivityKit
import Foundation

/// Shared attributes for Herald Live Activities.
/// Used by both the main app (to start/update activities) and the widget extension (to render them).
///
/// IMPORTANT: This file is compiled into BOTH the Herald app target and the HeraldWidgets
/// extension target. Keep ActivityAttributes and ContentState identical between the two —
/// mismatched fields cause silent decode failures in the widget extension.
struct KallistiActivityAttributes: ActivityAttributes, Sendable {
    /// Dynamic data — updated throughout the activity's lifetime.
    struct ContentState: Codable, Hashable, Sendable {
        // ── Session state ──────────────────────────────────────────────
        var status: String            // "Listening", "Thinking", "Working on that…"
        var toolName: String?         // e.g., "hermes_delegate", "vision_analyze"
        var elapsedSeconds: Int       // seconds since activity started
        var startDate: Date?          // used by Text(timerInterval:) for a live-ticking clock
        var sessionType: String       // "voice", "chat", "tool"
        var emoji: String?            // Contextual emoji for Dynamic Island

        // ── Gateway telemetry (Herald 2.3.0+) ──────────────────────────
        var gatewayConnected: Bool
        var activeQueries: Int
        var modelName: String?
        var version: String?
        var cpuPercent: Double
        var memoryUsedGb: Double
        var memoryTotalGb: Double
        var uptimeHours: Double
        var alertCount: Int

        // MARK: - Codable

        enum CodingKeys: String, CodingKey {
            case status, toolName, elapsedSeconds, startDate, sessionType, emoji
            case gatewayConnected, activeQueries, modelName, version
            case cpuPercent, memoryUsedGb, memoryTotalGb, uptimeHours, alertCount
        }

        /// Custom decoder that tolerates missing telemetry keys.
        /// Older builds encode ContentState without the gateway telemetry fields added in
        /// Herald 2.3.0. Without this custom init, the widget extension would throw
        /// `keyNotFound` and silently fail to render the Live Activity.
        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            status = try container.decode(String.self, forKey: .status)
            toolName = try container.decodeIfPresent(String.self, forKey: .toolName)
            elapsedSeconds = try container.decode(Int.self, forKey: .elapsedSeconds)
            startDate = try container.decodeIfPresent(Date.self, forKey: .startDate)
            sessionType = try container.decode(String.self, forKey: .sessionType)
            emoji = try container.decodeIfPresent(String.self, forKey: .emoji)

            // Telemetry fields — use decodeIfPresent so states encoded before
            // Herald 2.3.1 (without these keys) don't break decoding.
            gatewayConnected = try container.decodeIfPresent(Bool.self, forKey: .gatewayConnected) ?? false
            activeQueries = try container.decodeIfPresent(Int.self, forKey: .activeQueries) ?? 0
            modelName = try container.decodeIfPresent(String.self, forKey: .modelName)
            version = try container.decodeIfPresent(String.self, forKey: .version)
            cpuPercent = try container.decodeIfPresent(Double.self, forKey: .cpuPercent) ?? 0.0
            memoryUsedGb = try container.decodeIfPresent(Double.self, forKey: .memoryUsedGb) ?? 0.0
            memoryTotalGb = try container.decodeIfPresent(Double.self, forKey: .memoryTotalGb) ?? 0.0
            uptimeHours = try container.decodeIfPresent(Double.self, forKey: .uptimeHours) ?? 0.0
            alertCount = try container.decodeIfPresent(Int.self, forKey: .alertCount) ?? 0
        }

        /// Standard initializer for app-side activity creation.
        init(
            status: String,
            toolName: String? = nil,
            elapsedSeconds: Int = 0,
            startDate: Date? = nil,
            sessionType: String = "chat",
            emoji: String? = nil,
            gatewayConnected: Bool = false,
            activeQueries: Int = 0,
            modelName: String? = nil,
            version: String? = nil,
            cpuPercent: Double = 0.0,
            memoryUsedGb: Double = 0.0,
            memoryTotalGb: Double = 0.0,
            uptimeHours: Double = 0.0,
            alertCount: Int = 0
        ) {
            self.status = status
            self.toolName = toolName
            self.elapsedSeconds = elapsedSeconds
            self.startDate = startDate
            self.sessionType = sessionType
            self.emoji = emoji
            self.gatewayConnected = gatewayConnected
            self.activeQueries = activeQueries
            self.modelName = modelName
            self.version = version
            self.cpuPercent = cpuPercent
            self.memoryUsedGb = memoryUsedGb
            self.memoryTotalGb = memoryTotalGb
            self.uptimeHours = uptimeHours
            self.alertCount = alertCount
        }
    }

    /// Immutable for the lifetime of the activity.
    var agentName: String = "Herald"
}
