//
//  SubagentPreviewSheet.swift
//  Kallisti
//
//  What the children of a turn are doing. The bar above the composer says how
//  many there are; this is the answer to "which ones, and how far along".
//

import SwiftUI

struct SubagentPreviewSheet: View {
    let rows: [ErisSubagent]
    let onDismiss: () -> Void

    var body: some View {
        NavigationStack {
            List {
                if rows.isEmpty {
                    ContentUnavailableView(
                        "No subagents",
                        systemImage: "person.2",
                        description: Text("A turn that dispatches children lists them here while they work.")
                    )
                } else {
                    ForEach(rows) { row in
                        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                            HStack(spacing: Design.Spacing.sm) {
                                Image(systemName: icon(for: row))
                                    .foregroundStyle(color(for: row))
                                Text(row.label)
                                    .font(Design.Typography.body)
                                    .foregroundStyle(Design.Colors.foreground)
                                Spacer()
                                Text(row.status.uppercased())
                                    .font(.system(.caption2, design: .monospaced))
                                    .foregroundStyle(color(for: row))
                            }

                            HStack(spacing: Design.Spacing.md) {
                                if let elapsed = row.elapsedSecs {
                                    Text(String(format: "%d:%02d", elapsed / 60, elapsed % 60))
                                        .font(.system(.caption2, design: .monospaced))
                                }
                                if let checkin = row.checkinSecs {
                                    Text(String(format: "check-in %d:%02d", checkin / 60, checkin % 60))
                                        .font(.system(.caption2, design: .monospaced))
                                }
                                if let code = row.errorCode {
                                    Text(code)
                                        .font(.system(.caption2, design: .monospaced))
                                        .foregroundStyle(Design.Colors.warning)
                                }
                            }
                            .foregroundStyle(Design.Colors.secondaryForeground)

                            if let result = row.result, !result.isEmpty {
                                Text(result)
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                    .lineLimit(4)
                            }
                        }
                        .padding(.vertical, Design.Spacing.xxs)
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
