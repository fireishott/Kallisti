import SwiftUI
import WidgetKit

/// Health metrics grid — steps, calories, sleep, heart rate.
struct HeraldHealthWidget: Widget {
    let kind = "HeraldHealth"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: HeraldTimelineProvider()) { entry in
            HeraldHealthView(entry: entry)
                .containerBackground(for: .widget) {
                    Color(.systemBackground)
                }
        }
        .configurationDisplayName("Herald Health")
        .description("Daily health metrics at a glance.")
        .supportedFamilies([.systemMedium])
    }
}

// MARK: - Views

private struct HeraldHealthView: View {
    let entry: HeraldWidgetEntry

    var body: some View {
        VStack(spacing: 8) {
            HStack {
                HeraldBrandIcon(size: 16)
                Text("Health · Today")
                    .font(.system(.caption2, design: .monospaced))
                    .textCase(.uppercase)
                    .tracking(1.2)
                    .foregroundStyle(.primary)
                Spacer()
                Text(entry.data.updatedAt, style: .relative)
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.tertiary)
            }

            HStack(spacing: 12) {
                metricCard(
                    icon: "figure.walk",
                    label: "Steps",
                    value: entry.data.steps.map { formatNumber($0) } ?? "--"
                )
                metricCard(
                    icon: "flame.fill",
                    label: "Calories",
                    value: entry.data.activeCalories.map { formatNumber($0) } ?? "--"
                )
                metricCard(
                    icon: "bed.double.fill",
                    label: "Sleep",
                    value: entry.data.sleepHours.map { String(format: "%.1fh", $0) } ?? "--"
                )
                metricCard(
                    icon: "heart.fill",
                    label: "Heart",
                    value: entry.data.heartRate.map { "\($0)" } ?? "--"
                )
            }
        }
        .widgetURL(URL(string: "herald://health"))
    }

    private func metricCard(icon: String, label: String, value: String) -> some View {
        VStack(spacing: 4) {
            Image(systemName: icon)
                .font(.title3)
                .foregroundStyle(HeraldBrand.foreground)
            Text(value)
                .font(.subheadline.weight(.semibold).monospacedDigit())
                .foregroundStyle(.primary)
            Text(label)
                .font(.system(.caption2, design: .monospaced))
                .textCase(.uppercase)
                .tracking(1.0)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
    }

    private func formatNumber(_ n: Int) -> String {
        if n >= 1000 {
            return String(format: "%.1fk", Double(n) / 1000.0)
        }
        return "\(n)"
    }
}

// MARK: - Previews

#Preview("Medium", as: .systemMedium) {
    HeraldHealthWidget()
} timeline: {
    HeraldWidgetEntry.placeholder
    HeraldWidgetEntry(date: .now, data: .empty)
}
