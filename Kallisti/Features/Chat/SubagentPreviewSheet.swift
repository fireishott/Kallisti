//
//  SubagentPreviewSheet.swift
//  Kallisti
//
//  What the children of a turn are doing. The bar above the composer says how
//  many there are; this is the answer to "which ones, and how far along".
//
//  The rows are read from the store rather than passed in, so the sheet updates
//  on the same two-second poll that feeds the count. A running row can be opened
//  to see everything the child has produced so far.
//

import SwiftUI

struct SubagentPreviewSheet: View {
    @Environment(ChatStore.self) private var chatStore
    let onDismiss: () -> Void

    @State private var expanded: Set<String> = []

    var body: some View {
        NavigationStack {
            List {
                if chatStore.subagents.isEmpty {
                    ContentUnavailableView(
                        "No subagents",
                        systemImage: "person.2",
                        description: Text("A turn that dispatches children lists them here while they work.")
                    )
                } else {
                    ForEach(chatStore.subagents) { row in
                        Button {
                            if expanded.contains(row.id) {
                                expanded.remove(row.id)
                            } else {
                                expanded.insert(row.id)
                            }
                        } label: {
                            summary(row)
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .navigationTitle("Subagents")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { onDismiss() }
                }
            }
        }
    }

    @ViewBuilder
    private func summary(_ row: ErisSubagent) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: icon(for: row))
                    .foregroundStyle(color(for: row))
                Text(row.label)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
                    .multilineTextAlignment(.leading)
                Spacer()
                Text(row.status.uppercased())
                    .font(.system(.caption2, design: .monospaced))
                    .foregroundStyle(color(for: row))
                Image(systemName: expanded.contains(row.id) ? "chevron.down" : "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }

            HStack(spacing: Design.Spacing.md) {
                if let elapsed = row.elapsedSecs {
                    Text(String(format: "%d:%02d", elapsed / 60, elapsed % 60))
                }
                if let checkin = row.checkinSecs {
                    Text(String(format: "check-in %d:%02d", checkin / 60, checkin % 60))
                }
                if let code = row.errorCode {
                    Text(code).foregroundStyle(Design.Colors.warning)
                }
            }
            .font(.system(.caption2, design: .monospaced))
            .foregroundStyle(Design.Colors.secondaryForeground)

            if expanded.contains(row.id) {
                if let result = row.result, !result.isEmpty {
                    Text(result)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else if row.isActive {
                    Text("Working. The output appears here as the child produces it.")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                } else {
                    Text("This child produced no text.")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            } else if let result = row.result, !result.isEmpty {
                Text(result)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .lineLimit(2)
            }
        }
        .contentShape(Rectangle())
        .padding(.vertical, Design.Spacing.xxs)
    }

    private func icon(for row: ErisSubagent) -> String {
        switch row.status {
        case "running": "circle.dotted"
        case "queued": "hourglass"
        case "succeeded": "checkmark.circle"
        case "failed": "exclamationmark.triangle"
        default: "circle"
        }
    }

    private func color(for row: ErisSubagent) -> Color {
        switch row.status {
        case "running", "queued": Design.Brand.accent
        case "succeeded": Design.Colors.secondaryForeground
        case "failed": Design.Colors.warning
        default: Design.Colors.secondaryForeground
        }
    }
}
