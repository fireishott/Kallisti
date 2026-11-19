//
//  KallistiComplication.swift
//  KallistiWatchWidgets
//
//  The glance: the last line of the answer and the state word, on all four
//  accessory families. A tap opens the conversation, because a complication that
//  opens the app to a menu is a complication nobody uses twice.
//

import SwiftUI
import WidgetKit

struct KallistiEntry: TimelineEntry {
    let date: Date
    let line: String
    let stateWord: String
}

struct KallistiProvider: TimelineProvider {
    func placeholder(in context: Context) -> KallistiEntry {
        KallistiEntry(date: .now, line: "Ready when you are.", stateWord: "idle")
    }

    func getSnapshot(in context: Context, completion: @escaping (KallistiEntry) -> Void) {
        completion(current())
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<KallistiEntry>) -> Void) {
        // One entry. The app reloads on a turn start and on a settle; the fifteen
        // minutes is the floor for a watch whose app has not run in hours.
        let floor = Date().addingTimeInterval(15 * 60)
        completion(Timeline(entries: [current()], policy: .after(floor)))
    }

    private func current() -> KallistiEntry {
        let snapshot = WatchStateSnapshot.read()
        return KallistiEntry(
            date: .now,
            line: snapshot.lastLine.isEmpty ? "Ready when you are." : snapshot.lastLine,
            stateWord: snapshot.stateWord
        )
    }
}

struct KallistiComplication: Widget {
    static let kind = "KallistiComplication"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: Self.kind, provider: KallistiProvider()) { entry in
            KallistiComplicationView(entry: entry)
                .widgetURL(URL(string: "kallisti://talk"))
                .containerBackground(for: .widget) { AccessoryWidgetBackground() }
        }
        .configurationDisplayName("Kallisti")
        .description("The last answer, and what the wrist is doing.")
        .supportedFamilies([
            .accessoryCircular, .accessoryRectangular, .accessoryInline, .accessoryCorner,
        ])
    }
}

struct KallistiComplicationView: View {
    @Environment(\.widgetFamily) private var family
    let entry: KallistiEntry

    var body: some View {
        switch family {
        case .accessoryInline:
            // The only place the state word fits; a line of text is all the
            // inline family draws.
            Text("Kallisti · \(entry.stateWord)")
        case .accessoryCircular:
            VStack(spacing: 0) {
                Text("K").font(.title3.bold())
                Text(entry.stateWord)
                    .font(.system(size: 9))
                    .minimumScaleFactor(0.7)
                    .lineLimit(1)
            }
        case .accessoryCorner:
            Text("K")
                .font(.headline)
                .widgetLabel {
                    Text(entry.stateWord)
                }
        default:
            VStack(alignment: .leading, spacing: 1) {
                Text(entry.line)
                    .font(.caption2)
                    .lineLimit(2)
                Text(entry.stateWord)
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
    }
}
