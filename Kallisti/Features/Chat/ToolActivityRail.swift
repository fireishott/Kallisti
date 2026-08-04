import SwiftUI

/// A compact, live-rotating view showing what tools Herald is using in real time.
///
/// **Streaming**: cycles through tool labels one at a time with animated transitions.
/// **Finished**: shows a collapsed summary that expands to the full timeline on tap.
struct ToolActivityRail: View {
    let activities: [ToolActivity]
    let isStreaming: Bool

    @State private var isExpanded = false

    private var latestActivity: ToolActivity? {
        activities.last(where: { $0.isActive }) ?? activities.last
    }

    var body: some View {
        if !activities.isEmpty {
            if isStreaming {
                liveIndicator
            } else {
                finishedSummary
            }
        }
    }

    // MARK: - Live Streaming Indicator

    private var liveIndicator: some View {
        HStack(spacing: Design.Spacing.xs) {
            ProgressView()
                .controlSize(.mini)
                .tint(Design.Colors.secondaryForeground)

            if let latest = latestActivity {
                Text(latest.label)
                    .brandEyebrow()
                    .lineLimit(1)
                    .id(latest.id)
                    .transition(.asymmetric(
                        insertion: .move(edge: .bottom).combined(with: .opacity),
                        removal: .move(edge: .top).combined(with: .opacity)
                    ))
                    .animation(Design.Motion.quickResponse, value: latest.id)
            }
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, Design.Spacing.xxs + 1)
        .background(Design.Colors.surface)
        .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
        .clipShape(Capsule())
    }

    // MARK: - Finished Summary (expandable)

    private var finishedSummary: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            Button {
                guard activities.count > 1 else { return }
                withAnimation(Design.Motion.quickResponse) {
                    isExpanded.toggle()
                }
            } label: {
                HStack(spacing: Design.Spacing.xs) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 10))
                        .foregroundStyle(Design.Colors.success)

                    Text("\(activities.count) tool\(activities.count == 1 ? "" : "s") used")
                        .brandEyebrow()

                    if activities.count > 1 {
                        Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                            .font(.system(size: 8, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.vertical, Design.Spacing.xxs + 1)
                .background(Design.Colors.surface)
                .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
                .clipShape(Capsule())
            }
            .buttonStyle(.plain)

            if isExpanded {
                expandedTimeline
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .animation(Design.Motion.quickResponse, value: isExpanded)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Tools: \(activities.map(\.label).joined(separator: ", "))")
    }

    // MARK: - Expanded Timeline

    private var expandedTimeline: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
            ForEach(activities) { activity in
                HStack(spacing: Design.Spacing.xs) {
                    Circle()
                        .fill(Design.Colors.foreground)
                        .frame(width: 5, height: 5)

                    Text(activity.label)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.foreground)
                        .lineLimit(1)

                    Spacer()

                    Text(activity.startedAt, style: .time)
                        .brandEyebrow(Design.Colors.tertiaryForeground)
                }
                .padding(.horizontal, Design.Spacing.xs)
                .padding(.vertical, Design.Spacing.xxxs)
            }
        }
        .padding(.vertical, Design.Spacing.xxs)
        .padding(.horizontal, Design.Spacing.xxs)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
    }
}
