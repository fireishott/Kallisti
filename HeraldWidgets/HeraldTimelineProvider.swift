import Foundation
import WidgetKit

/// Timeline entry backed by the App Group shared data snapshot.
struct HeraldWidgetEntry: TimelineEntry {
    let date: Date
    let data: HeraldWidgetData

    static let placeholder = HeraldWidgetEntry(
        date: .now,
        data: HeraldWidgetData(
            hostName: "Herald",
            hostOnline: true,
            lastMessagePreview: "Good morning! How can I help?",
            lastMessageSender: "assistant",
            lastMessageAt: .now,
            voiceSessionActive: false,
            steps: 4_230,
            activeCalories: 185,
            sleepHours: 7.4,
            heartRate: 68,
            updatedAt: .now
        )
    )
}

/// Reads the latest snapshot from the App Group shared container.
struct HeraldTimelineProvider: TimelineProvider {
    private static let appGroupID: String = {
        if let custom = Bundle.main.object(forInfoDictionaryKey: "APP_GROUP_ID") as? String, !custom.isEmpty {
            return custom
        }
        return "group.net.fihonline.herald"
    }()
    private static let dataKey = "herald.widget.data"

    func placeholder(in context: Context) -> HeraldWidgetEntry {
        .placeholder
    }

    func getSnapshot(in context: Context, completion: @escaping (HeraldWidgetEntry) -> Void) {
        completion(HeraldWidgetEntry(date: .now, data: readData()))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<HeraldWidgetEntry>) -> Void) {
        let entry = HeraldWidgetEntry(date: .now, data: readData())
        // Refresh every 15 minutes; immediate refreshes are triggered by
        // WidgetCenter.shared.reloadAllTimelines() in the main app.
        let nextRefresh = Calendar.current.date(byAdding: .minute, value: 15, to: .now) ?? .now
        let timeline = Timeline(entries: [entry], policy: .after(nextRefresh))
        completion(timeline)
    }

    private func readData() -> HeraldWidgetData {
        guard let defaults = UserDefaults(suiteName: Self.appGroupID),
              let raw = defaults.data(forKey: Self.dataKey),
              let decoded = try? JSONDecoder().decode(HeraldWidgetData.self, from: raw)
        else {
            return .empty
        }
        return decoded
    }
}
