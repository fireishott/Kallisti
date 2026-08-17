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

    private var hasExpandableDetail: Bool {
        activities.contains { activity in
            !(activity.argsPreview ?? "").isEmpty
                || !(activity.resultPreview ?? "").isEmpty
                || !activity.liveOutput.isEmpty
        }
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
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
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

            if let latest = latestActivity {
                // Build 128.58: finished tools render terminal-style too.
                // Tools like read_file return results instead of stdout, so
                // liveOutput may stay empty while resultPreview carries the
                // actual output - that still belongs in the terminal view.
                if !latest.liveOutput.isEmpty {
                    TerminalOutputView(
                        text: latest.liveOutput,
                        isActive: latest.isActive,
                        maxChars: 48 * 1024
                    )
                    .transition(.opacity)
                } else if let result = latest.resultPreview, !result.isEmpty {
                    TerminalOutputView(
                        text: result,
                        isActive: false,
                        maxChars: 48 * 1024
                    )
                    .transition(.opacity)
                } else {
                    ToolCallBubbleView(
                        name: latest.name ?? latest.label,
                        args: latest.argsPreview,
                        result: latest.resultPreview
                    )
                }
            }
        }
    }

    // MARK: - Finished Summary (expandable)

    private var finishedSummary: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            Button {
                guard activities.count > 1 || hasExpandableDetail else { return }
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

                    if activities.count > 1 || hasExpandableDetail {
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
                // Build 128.58: the expanded timeline uses the terminal look
                // for anything with output - streamed stdout OR tool result -
                // instead of raw JSON argument dumps.
                if !activity.liveOutput.isEmpty || !(activity.resultPreview ?? "").isEmpty {
                    VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
                        HStack(spacing: Design.Spacing.xs) {
                            if activity.isActive {
                                ProgressView()
                                    .controlSize(.mini)
                                    .tint(Design.Colors.secondaryForeground)
                            } else {
                                Image(systemName: activity.isError ? "xmark.circle.fill" : "checkmark.circle.fill")
                                    .font(.system(size: 10))
                                    .foregroundStyle(activity.isError ? Design.Colors.danger : Design.Colors.success)
                            }
                            Text(activity.name ?? activity.label)
                                .brandEyebrow()
                                .lineLimit(1)
                            Spacer()
                            if let ms = activity.durationMs {
                                Text("\(ms) ms")
                                    .font(.system(.caption2, design: .monospaced))
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                            }
                        }
                        TerminalOutputView(
                            text: activity.liveOutput.isEmpty ? (activity.resultPreview ?? "") : activity.liveOutput,
                            isActive: activity.isActive,
                            maxChars: 48 * 1024
                        )
                    }
                } else {
                    ToolCallBubbleView(
                        name: activity.name ?? activity.label,
                        args: activity.argsPreview,
                        result: activity.resultPreview
                    )
                }
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
