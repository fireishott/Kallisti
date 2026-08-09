import SwiftUI

/// View for reviewing and correcting recognized text and detected directives.
struct RecognizedTextReviewView: View {
    let recognition: NoteRecognition
    let directives: [NoteDirective]
    var onCorrectedTextChanged: ((String) -> Void)?

    @State private var correctedText: String
    @State private var isEditing = false

    init(recognition: NoteRecognition, directives: [NoteDirective], onCorrectedTextChanged: ((String) -> Void)? = nil) {
        self.recognition = recognition
        self.directives = directives
        self.onCorrectedTextChanged = onCorrectedTextChanged
        _correctedText = State(initialValue: recognition.effectiveText)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Header
            HStack {
                Label("Recognized Text", systemImage: "text.viewfinder")
                    .font(.headline)
                Spacer()
                Button(isEditing ? "Done" : "Edit") {
                    isEditing.toggle()
                    if !isEditing {
                        onCorrectedTextChanged?(correctedText)
                    }
                }
                .font(.subheadline)
            }

            // Engine info
            HStack {
                Image(systemName: "cpu")
                    .font(.caption)
                Text(recognition.engine == .visionAccurate ? "Accurate" : "Fast")
                    .font(.caption)
                Text("·")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("\(recognition.languages.joined(separator: ", "))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .foregroundStyle(.secondary)

            // Text editor or display
            if isEditing {
                TextEditor(text: $correctedText)
                    .font(.body)
                    .frame(minHeight: 120)
                    .overlay(
                        RoundedRectangle(cornerRadius: 8)
                            .stroke(Color.secondary.opacity(0.2), lineWidth: 1)
                    )
            } else {
                Text(recognition.effectiveText)
                    .font(.body)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                    .background(Color.secondary.opacity(0.05))
                    .cornerRadius(8)
            }

            // Detected directives
            if !directives.isEmpty {
                Divider()

                Label("Detected Commands", systemImage: "tag")
                    .font(.headline)

                ForEach(directives) { directive in
                    HStack {
                        Image(systemName: "tag.fill")
                            .foregroundStyle(Design.Brand.accent)
                            .font(.caption)
                        Text("#\(directive.command.rawValue)")
                            .font(.subheadline)
                            .fontWeight(.medium)
                        if !directive.arguments.isEmpty {
                            Text(directive.arguments)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
        }
        .padding()
    }
}
