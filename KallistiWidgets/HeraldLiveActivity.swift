import ActivityKit
import SwiftUI
import UIKit
import WidgetKit

// Brand palette mirrored from KallistiTheme — widget target doesn't link the
// main app's Design module, so we keep a minimal palette inline.
enum KallistiBrand {
    static let accent = Color(red: 0.784, green: 0.800, blue: 0.824)    // platinum #C8CCD2
    static let foreground = Color(red: 0.941, green: 0.949, blue: 0.961) // bone #F0F2F5
    static let surface = Color(red: 0.110, green: 0.114, blue: 0.133)   // surface #1C1D22
    static let border = Color(red: 0.165, green: 0.169, blue: 0.200, opacity: 0.35)
}

struct KallistiBrandIcon: View {
    let size: CGFloat
    var fallbackSymbol: String = "waveform"
    var fallbackTint: Color = HeraldBrand.accent
    var backgroundTint: Color? = nil
    var cornerRadius: CGFloat? = nil

    var body: some View {
        if let uiImage = Self.loadImage() {
            Image(uiImage: uiImage)
                .resizable()
                .renderingMode(.original)
                .aspectRatio(contentMode: .fit)
                .frame(width: size, height: size)
                .clipShape(RoundedRectangle(cornerRadius: cornerRadius ?? size * 0.22))
                .ifLet(backgroundTint) { view, tint in
                    view.background(tint, in: RoundedRectangle(cornerRadius: cornerRadius ?? size * 0.22))
                }
        } else {
            Image(systemName: fallbackSymbol)
                .font(.system(size: size * 0.7, weight: .medium))
                .foregroundStyle(fallbackTint)
                .frame(width: size, height: size)
                .ifLet(backgroundTint) { view, tint in
                    view.background(tint, in: Circle())
                }
        }
    }

    private static func loadImage() -> UIImage? {
        if let image = UIImage(named: "AppIcon60x60", in: Bundle.main, compatibleWith: nil) {
            return image
        }

        let containerAppURL = Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        if let appBundle = Bundle(url: containerAppURL),
           let image = UIImage(named: "AppIcon60x60", in: appBundle, compatibleWith: nil) {
            return image
        }

        return nil
    }
}

extension View {
    @ViewBuilder
    func ifLet<T, Content: View>(_ value: T?, transform: (Self, T) -> Content) -> some View {
        if let value {
            transform(self, value)
        } else {
            self
        }
    }
}

struct KallistiLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: KallistiActivityAttributes.self) { context in
            // Lock Screen layout
            lockScreenView(context: context)
        } dynamicIsland: { context in
            DynamicIsland {
                // Expanded view (long press on Dynamic Island)
                DynamicIslandExpandedRegion(.leading) {
                    KallistiBrandIcon(size: 28)
                }
                DynamicIslandExpandedRegion(.center) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(context.attributes.agentName)
                            .font(.system(.caption2, design: .monospaced))
                            .textCase(.uppercase)
                            .tracking(1.2)
                            .foregroundStyle(.white.opacity(0.7))
                        Text(context.state.status)
                            .font(.subheadline)
                            .italic()
                            .foregroundStyle(.white)
                    }
                }
                DynamicIslandExpandedRegion(.trailing) {
                    if let tool = context.state.toolName {
                        Text(_ToolNameSanitizer.displayLabel(for: tool))
                            .font(.system(.caption2, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.tail)
                            .foregroundStyle(HeraldBrand.accent.opacity(0.8))
                    }
                }
            } compactLeading: {
                // Compact left side of Dynamic Island
                if let emoji = context.state.emoji {
                    Text(emoji).font(.system(size: 14))
                } else {
                    KallistiBrandIcon(size: 14)
                }
            } compactTrailing: {
                // Compact right side
                Text(context.state.status.prefix(12))
                    .font(.system(.caption2, design: .monospaced))
                    .textCase(.uppercase)
                    .tracking(1.0)
                    .foregroundStyle(.white.opacity(0.85))
            } minimal: {
                // Minimal (when multiple Live Activities compete)
                if let emoji = context.state.emoji {
                    Text(emoji).font(.system(size: 16))
                } else {
                    KallistiBrandIcon(size: 16)
                }
            }
        }
        .supplementalActivityFamilies([.small])
    }

    @ViewBuilder
    private func lockScreenView(context: ActivityViewContext<KallistiActivityAttributes>) -> some View {
        HStack(spacing: 12) {
            KallistiBrandIcon(
                size: 44,
                backgroundTint: HeraldBrand.surface,
                cornerRadius: 12
            )

            VStack(alignment: .leading, spacing: 2) {
                Text(context.attributes.agentName)
                    .font(.system(.caption2, design: .monospaced))
                    .textCase(.uppercase)
                    .tracking(1.2)
                    .foregroundStyle(.secondary)

                // Build 32: status and detail are sanitized by LiveActivityService
                // to a small enum.  Line limits are defense-in-depth — even if a
                // future code path writes raw text, it can never expand the card.
                Text(context.state.status)
                    .font(.subheadline)
                    .italic()
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .foregroundStyle(.primary)

                if let tool = context.state.toolName {
                    Text(_ToolNameSanitizer.displayLabel(for: tool))
                        .font(.system(.caption, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .foregroundStyle(HeraldBrand.accent)
                }
            }

            Spacer()

            // Use the native timer when a start date is available —
            // this ticks in real-time without needing Live Activity updates.
            if let start = context.state.startDate {
                Text(timerInterval: start...Date.distantFuture, countsDown: false)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.trailing)
            } else if context.state.elapsedSeconds > 0 {
                Text(formatDuration(context.state.elapsedSeconds))
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
        }
        .padding()
        .fixedSize(horizontal: false, vertical: true)
    }

    private func formatDuration(_ seconds: Int) -> String {
        let m = seconds / 60
        let s = seconds % 60
        return String(format: "%d:%02d", m, s)
    }
}

// KEEP IN SYNC with HeraldSupport/ToolNameSanitizer.swift
// (Widget extension cannot import HeraldSupport module, so a private copy lives here.)
private enum _ToolNameSanitizer {
    static func displayLabel(for raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "Working" }
        let identifier = trimmed.range(of: #"^[A-Za-z][A-Za-z0-9_.\-]{0,63}$"#, options: .regularExpression) != nil
        if identifier {
            switch trimmed.lowercased() {
            case "view_file", "read_file", "cat": return "Reading a file"
            case "search", "grep", "find": return "Searching"
            case "date": return "Checking the time"
            default:
                if trimmed.count > 25 { return String(trimmed.prefix(25)) + "\u{2026}" }
                return trimmed
            }
        }
        return "Running a command"
    }
}

// Previews are in Herald/Features/Talk/LiveActivityPreviews.swift
// (Widget extension targets cannot host previews.)
