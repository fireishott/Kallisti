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