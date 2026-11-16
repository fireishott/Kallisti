import SwiftUI

/// Build 128.76: interactive clarify card.
///
/// When the agent parks a turn on a clarify question (`clarify.request` WS
/// event), the chat shows this card above the composer so the question is
/// answerable: tappable choices (with the agent's recommended option first,
/// matching the server's `(Recommended)` labeling), an "Other" free-text
/// row for typed answers, and a cancel action that dismisses the card
/// (the parked turn still times out server-side - see clarify_timeout).
///
/// Answering calls `ChatStore.submitClarifyAnswer`, which sends
/// `clarify.respond` over the gateway WS to unblock the turn.
struct ClarifyCardView: View {
    let clarify: PendingClarify
    let onSubmit: (String) -> Void
    let onDismiss: () -> Void

    @State private var typedAnswer = ""

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            // Header row
            HStack(alignment: .firstTextBaseline) {
                Text("◆ clarify")
                    .font(Design.Typography.code)
                    .foregroundStyle(Design.Colors.accent)
                Spacer()
                Button(action: onDismiss) {
                    Image(systemName: "xmark")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }

            // Question
            Text(clarify.question)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(.primary)
                .fixedSize(horizontal: false, vertical: true)

            // Choices or free text
            if let choices = clarify.choices, !choices.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(choices.enumerated()), id: \.offset) { index, choice in
                        Button {
                            onSubmit(choice)
                        } label: {
                            HStack(spacing: 8) {
                                Text("\(index + 1).")
                                    .font(Design.Typography.code)
                                    .foregroundStyle(.secondary)
                                Text(choice)
                                    .font(.system(size: 14, weight: .medium))
                                    .foregroundStyle(.primary)
                                    .multilineTextAlignment(.leading)
                                Spacer(minLength: 4)
                                if index == 0 && choices.count > 1 {
                                    Text("(Recommended)")
                                        .font(Design.Typography.codeSmall)
                                        .foregroundStyle(Design.Colors.accent.opacity(0.8))
                                }
                            }
                            .padding(.vertical, 10)
                            .padding(.horizontal, 12)
                            .background(
                                RoundedRectangle(cornerRadius: 8)
                                    .fill(Design.Colors.surface.opacity(0.6))
                            )
                            .overlay(
                                RoundedRectangle(cornerRadius: 8)
                                    .stroke(Design.Colors.border, lineWidth: 1)
                            )
                        }
                        .buttonStyle(.plain)
                    }
                }

                Divider()
                    .overlay(Color.secondary.opacity(0.3))

                // Other (type answer) row
                HStack(spacing: 8) {
                    TextField("Type your answer...", text: $typedAnswer)
                        .textFieldStyle(.plain)
                        .font(.system(size: 14))
                        .submitLabel(.done)
                        .onSubmit {
                            let answer = typedAnswer.trimmingCharacters(in: .whitespacesAndNewlines)
                            guard !answer.isEmpty else { return }
                            onSubmit(answer)
                        }
                    Button("Send") {
                        let answer = typedAnswer.trimmingCharacters(in: .whitespacesAndNewlines)
                        guard !answer.isEmpty else { return }
                        onSubmit(answer)
                    }
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(Design.Colors.accent)
                    .disabled(typedAnswer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .opacity(typedAnswer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? 0.5 : 1)
                }
            } else {
                // Open-ended clarify: no choices, just the free-text answer.
                HStack(spacing: 8) {
                    TextField("Type your answer...", text: $typedAnswer)
                        .textFieldStyle(.plain)
                        .font(.system(size: 14))
                        .submitLabel(.done)
                        .onSubmit {
                            let answer = typedAnswer.trimmingCharacters(in: .whitespacesAndNewlines)
                            guard !answer.isEmpty else { return }
                            onSubmit(answer)
                        }
                    Button("Send") {
                        let answer = typedAnswer.trimmingCharacters(in: .whitespacesAndNewlines)
                        guard !answer.isEmpty else { return }
                        onSubmit(answer)
                    }
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(Design.Colors.accent)
                    .disabled(typedAnswer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .opacity(typedAnswer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? 0.5 : 1)
                }
            }
        }
        .padding(Design.Spacing.md)
        .background(
            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                .fill(Design.Colors.background)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                .stroke(Design.Colors.accent.opacity(0.5), lineWidth: 1)
        )
        .padding(.horizontal, Design.Spacing.md)
        .padding(.top, Design.Spacing.xs)
    }
}

// MARK: - Turn artifact: a diff

/// One changed file with its counts, expanding to the hunks the daemon sent.
///
/// The data comes from a turn.artifact.diff frame through ErisDiff, so the card draws
/// exactly what the daemon diffed - it does not diff anything itself. A turn with
/// no diff never builds one of these.
struct ErisDiffCard: View {
    let diff: ErisDiff
    @State private var expanded = false

    private var totals: (added: Int, removed: Int) { diff.totals }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                expanded.toggle()
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                        .font(.caption2.weight(.semibold))
                    Text(diff.files.count == 1 ? "1 file changed" : "\(diff.files.count) files changed")
                        .font(.caption.weight(.semibold))
                    Text("+\(totals.added)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.green)
                    Text("-\(totals.removed)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.red)
                    Spacer(minLength: 0)
                }
            }
            .buttonStyle(.plain)

            ForEach(diff.files, id: \.path) { file in
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(file.path)
                            .font(.caption.monospaced())
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer(minLength: 0)
                        Text("+\(file.added)")
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(.green)
                        Text("-\(file.removed)")
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(.red)
                    }
                    if expanded {
                        ForEach(Array(file.hunks.enumerated()), id: \.offset) { _, hunk in
                            Text("@\(hunk.oldStart) +\(hunk.newStart)")
                                .font(.caption2.monospaced())
                                .foregroundStyle(.secondary)
                            ForEach(Array(hunk.lines.enumerated()), id: \.offset) { _, line in
                                Text((line.kind == .add ? "+" : line.kind == .remove ? "-" : " ") + line.text)
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(colour(for: line.kind))
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                }
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 10).fill(.quaternary.opacity(0.35)))
    }

    private func colour(for kind: ErisDiffLine.Kind) -> Color {
        switch kind {
        case .add: return .green
        case .remove: return .red
        case .context: return .secondary
        }
    }
}
