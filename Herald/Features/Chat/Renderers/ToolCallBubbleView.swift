import SwiftUI

/// Renders a tool-call segment as a pill header with expandable args/result JSON.
struct ToolCallBubbleView: View {
    let name: String
    let args: String?
    let result: String?

    @State private var isExpanded = false

    private var hasDetail: Bool { args != nil || result != nil }
    private var displayName: String {
        name.replacingOccurrences(of: "_", with: " ").capitalized
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Pill header
            Button {
                if hasDetail {
                    withAnimation(.easeInOut(duration: 0.15)) {
                        isExpanded.toggle()
                    }
                }
            } label: {
                HStack(spacing: Design.Spacing.xs) {
                    Image(systemName: "wrench.and.screwdriver")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Design.Brand.accent)
                    Text(displayName)
                        .font(.system(.caption, design: .monospaced, weight: .semibold))
                        .foregroundStyle(Design.Colors.foreground)
                    Spacer()
                    if hasDetail {
                        Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                            .font(.system(size: 10))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.vertical, Design.Spacing.xs)
            }
            .buttonStyle(.plain)

            // Expandable args/result
            if isExpanded {
                Divider()
                    .background(Design.Colors.border)
                VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                    if let args {
                        Text("args")
                            .font(.system(.caption2, design: .monospaced, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Text(args)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(Design.Colors.foreground)
                            .textSelection(.enabled)
                    }
                    if let result {
                        Text("result")
                            .font(.system(.caption2, design: .monospaced, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Text(result)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(Design.Colors.foreground)
                            .textSelection(.enabled)
                    }
                }
                .padding(Design.Spacing.sm)
            }
        }
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                .stroke(Design.Brand.accent.opacity(0.4), lineWidth: 1)
        )
    }
}
