import SwiftUI

/// Shows the execution status of directives within an enrichment run.
struct DirectiveProgressView: View {
    let directives: [NoteDirective]
    let commandResults: [NoteCommandResult]
    let runStatus: NoteRunStatus.Status?
    let onRetry: ((NoteDirective) -> Void)?

    init(
        directives: [NoteDirective],
        commandResults: [NoteCommandResult] = [],
        runStatus: NoteRunStatus.Status? = nil,
        onRetry: ((NoteDirective) -> Void)? = nil
    ) {
        self.directives = directives
        self.commandResults = commandResults
        self.runStatus = runStatus
        self.onRetry = onRetry
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Label("Directives", systemImage: "tag.fill")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)

            ForEach(directives) { directive in
                directiveRow(directive)
            }
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
    }

    @ViewBuilder
    private func directiveRow(_ directive: NoteDirective) -> some View {
        let status = statusForDirective(directive)

        HStack(spacing: Design.Spacing.sm) {
            // Status icon
            statusIcon(for: status)

            // Directive info
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: Design.Spacing.xxs) {
                    Text("#\(directive.command.rawValue)")
                        .font(Design.Typography.callout)
                        .fontWeight(.medium)
                        .foregroundStyle(Design.Colors.foreground)

                    if !directive.arguments.isEmpty {
                        Text(directive.arguments)
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                    }
                }

                // Status label
                statusLabel(for: status)
            }

            Spacer()

            // Retry button for failed directives
            if case .failed = status, let onRetry {
                Button {
                    onRetry(directive)
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Brand.accent)
                }
                .accessibilityLabel("Retry directive")
            }
        }
        .padding(.vertical, Design.Spacing.xxs)
    }

    @ViewBuilder
    private func statusIcon(for status: DirectiveItemStatus) -> some View {
        switch status {
        case .running:
            ProgressView()
                .controlSize(.small)
                .tint(Design.Brand.accent)
        case .completed:
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(Design.Colors.success)
                .font(.system(size: Design.Size.iconSmall))
        case .failed:
            Image(systemName: "exclamationmark.circle.fill")
                .foregroundStyle(Design.Colors.danger)
                .font(.system(size: Design.Size.iconSmall))
        case .stale:
            Image(systemName: "arrow.triangle.2.circlepath")
                .foregroundStyle(Design.Colors.warning)
                .font(.system(size: Design.Size.iconSmall))
        case .pending:
            Image(systemName: "circle")
                .foregroundStyle(Design.Colors.secondaryForeground)
                .font(.system(size: Design.Size.iconSmall))
        }
    }

    @ViewBuilder
    private func statusLabel(for status: DirectiveItemStatus) -> some View {
        switch status {
        case .running:
            Text("Processing...")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Brand.accent)
        case .completed(let result):
            if let sectionIndex = result.sectionIndex {
                Text("Completed (section \(sectionIndex + 1))")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.success)
            } else {
                Text("Completed")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.success)
            }
        case .failed(let result):
            Text(result.errorText ?? "Failed")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.danger)
                .lineLimit(2)
        case .stale:
            Text("Source changed — re-run needed")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.warning)
        case .pending:
            Text("Pending")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
    }

    private func statusForDirective(_ directive: NoteDirective) -> DirectiveItemStatus {
        // If run is still active (queued/claimed), show running
        if runStatus == .queued || runStatus == .claimed {
            return .running
        }

        // Check command results for this directive
        if let result = commandResults.first(where: { $0.directiveId == directive.id }) {
            switch result.status {
            case .completed:
                return .completed(result)
            case .failed:
                return .failed(result)
            case .skipped:
                return .failed(NoteCommandResult(
                    directiveId: directive.id,
                    status: .failed,
                    errorText: "Skipped"
                ))
            case .pending:
                return .pending
            }
        }

        // If run completed but no result for this directive, it's stale
        if runStatus == .completed {
            return .stale
        }

        return .pending
    }
}

// MARK: - Directive Item Status

enum DirectiveItemStatus {
    case running
    case completed(NoteCommandResult)
    case failed(NoteCommandResult)
    case stale
    case pending
}
