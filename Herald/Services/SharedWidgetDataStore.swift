import Foundation
import WidgetKit

/// Reads and writes `HeraldWidgetData` to the App Group shared container.
/// The main app writes; the widget extension reads.
enum SharedWidgetDataStore {
    /// App Group identifier. Reads from the APP_GROUP_ID Info.plist key if set,
    /// otherwise falls back to the default. Self-hosted users who change their
    /// bundle identifier should set APP_GROUP_ID in their build settings or
    /// local xcconfig to match their App Group.
    static let appGroupID: String = {
        if let custom = Bundle.main.object(forInfoDictionaryKey: "APP_GROUP_ID") as? String, !custom.isEmpty {
            return custom
        }
        return "group.net.fihonline.herald"
    }()
    private static let dataKey = "herald.widget.data"

    // Coalesce bursts of reloads. During chat streaming the caller can write
    // widget data dozens of times per second; `reloadAllTimelines` is an
    // expensive IPC boundary and the widget only needs the latest state.
    private static let reloadQueue = DispatchQueue(label: "net.fihonline.herald.widgetReload")
    nonisolated(unsafe) private static var pendingReload = false
    nonisolated(unsafe) private static var lastReloadAt: Date?
    private static let reloadMinInterval: TimeInterval = 2.0

    static func write(_ data: HeraldWidgetData) {
        guard let defaults = UserDefaults(suiteName: appGroupID) else { return }
        guard let encoded = try? JSONEncoder().encode(data) else { return }
        defaults.set(encoded, forKey: dataKey)
        scheduleReload()
    }

    private static func scheduleReload() {
        reloadQueue.async {
            if pendingReload { return }
            let delay: TimeInterval
            if let last = lastReloadAt {
                let elapsed = Date().timeIntervalSince(last)
                delay = max(0, reloadMinInterval - elapsed)
            } else {
                delay = 0
            }
            pendingReload = true
            reloadQueue.asyncAfter(deadline: .now() + delay) {
                pendingReload = false
                lastReloadAt = .now
                WidgetCenter.shared.reloadAllTimelines()
            }
        }
    }

    static func read() -> HeraldWidgetData {
        guard let defaults = UserDefaults(suiteName: appGroupID),
              let data = defaults.data(forKey: dataKey),
              let decoded = try? JSONDecoder().decode(HeraldWidgetData.self, from: data)
        else {
            return .empty
        }
        return decoded
    }

    static func updateHealthMetrics(from samples: [HealthSnapshot.Sample]) {
        guard !samples.isEmpty else { return }

        var data = read()
        for sample in samples {
            switch sample.metric {
            case "steps":
                data.steps = Int(sample.value.rounded())
            case "active_calories":
                data.activeCalories = Int(sample.value.rounded())
            case "sleep_duration":
                data.sleepHours = sample.value
            case "heart_rate":
                data.heartRate = Int(sample.value.rounded())
            default:
                continue
            }
        }
        data.updatedAt = .now
        write(data)
    }
}
