import Foundation
import WidgetKit

/// Shared gateway status for Control Center widget display.
/// Written by the main app periodically and read by Control Widgets
/// to show gateway health without an HTTP round-trip.
///
/// Uses App Group UserDefaults for cross-process communication.
/// UserDefaults is documented as thread-safe by Apple.
final class GatewayState: @unchecked Sendable {
    static let shared = GatewayState()

    private let defaults: UserDefaults = {
        guard let suite = UserDefaults(suiteName: "group.net.fihonline.kallisti") else {
            fatalError("App Group 'group.net.fihonline.kallisti' not available — check provisioning profile.")
        }
        return suite
    }()

    // MARK: - Gateway status

    var isConnected: Bool {
        defaults.bool(forKey: "gw.connected")
    }

    var activeJobs: Int {
        defaults.integer(forKey: "gw.activeJobs")
    }

    var model: String? {
        defaults.string(forKey: "gw.model")
    }

    var version: String? {
        defaults.string(forKey: "gw.version")
    }

    var uptimeSeconds: Int {
        defaults.integer(forKey: "gw.uptimeSeconds")
    }

    var lastUpdated: Date? {
        defaults.object(forKey: "gw.lastUpdated") as? Date
    }

    // MARK: - Telemetry

    var cpuPercent: Double {
        defaults.double(forKey: "gw.cpuPercent")
    }

    var memoryUsedGb: Double {
        defaults.double(forKey: "gw.memoryUsedGb")
    }

    var memoryTotalGb: Double {
        defaults.double(forKey: "gw.memoryTotalGb")
    }

    var alertCount: Int {
        defaults.integer(forKey: "gw.alertCount")
    }

    // MARK: - Update

    /// Update the gateway state from the main app. Each parameter updates independently;
    /// pass nil to leave a field unchanged.
    func update(
        connected: Bool? = nil,
        activeJobs: Int? = nil,
        model: String? = nil,
        version: String? = nil,
        uptimeSeconds: Int? = nil,
        cpuPercent: Double? = nil,
        memoryUsedGb: Double? = nil,
        memoryTotalGb: Double? = nil,
        alertCount: Int? = nil
    ) {
        var changed = false
        if let v = connected { defaults.set(v, forKey: "gw.connected"); changed = true }
        if let v = activeJobs { defaults.set(v, forKey: "gw.activeJobs"); changed = true }
        if let v = model { defaults.set(v, forKey: "gw.model"); changed = true }
        if let v = version { defaults.set(v, forKey: "gw.version"); changed = true }
        if let v = uptimeSeconds { defaults.set(v, forKey: "gw.uptimeSeconds"); changed = true }
        if let v = cpuPercent { defaults.set(v, forKey: "gw.cpuPercent"); changed = true }
        if let v = memoryUsedGb { defaults.set(v, forKey: "gw.memoryUsedGb"); changed = true }
        if let v = memoryTotalGb { defaults.set(v, forKey: "gw.memoryTotalGb"); changed = true }
        if let v = alertCount { defaults.set(v, forKey: "gw.alertCount"); changed = true }

        if changed {
            defaults.set(Date(), forKey: "gw.lastUpdated")
            WidgetCenter.shared.reloadAllTimelines()
        }
    }

    /// Clear all gateway state (called on disconnect).
    func clear() {
        defaults.removeObject(forKey: "gw.connected")
        defaults.removeObject(forKey: "gw.activeJobs")
        defaults.removeObject(forKey: "gw.model")
        defaults.removeObject(forKey: "gw.version")
        defaults.removeObject(forKey: "gw.uptimeSeconds")
        defaults.removeObject(forKey: "gw.lastUpdated")
        defaults.removeObject(forKey: "gw.cpuPercent")
        defaults.removeObject(forKey: "gw.memoryUsedGb")
        defaults.removeObject(forKey: "gw.memoryTotalGb")
        defaults.removeObject(forKey: "gw.alertCount")
        WidgetCenter.shared.reloadAllTimelines()
    }
}
