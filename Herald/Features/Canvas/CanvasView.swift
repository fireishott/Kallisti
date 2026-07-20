import SwiftUI

/// Editable canvas panel for AI-generated code artifacts.
/// Shown as a sheet on iPhone, tab on iPad.
/// Close (X) dismisses without deleting. Clear/Delete removes the artifact.
struct CanvasView: View {
    @Bindable var store: HeraldCanvasStore
    var onDismiss: (() -> Void)? = nil

    @State private var editedContent: String = ""
    @State private var copied = false
    @State private var showClearConfirmation = false

    var body: some View {
        VStack(spacing: 0) {
            // Toolbar
            HStack {
                if let artifact = store.activeArtifact {
                    Label(artifact.type.displayName, systemImage: "doc.text")
                        .font(.system(.caption, weight: .semibold))
                        .foregroundStyle(Design.Brand.accent)
                }
                Spacer()
                Button {
                    UIPasteboard.general.string = editedContent
                    copied = true
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { copied = false }
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(.system(size: 13))
                        .foregroundStyle(copied ? .green : Design.Colors.secondaryForeground)
                }
                .accessibilityLabel("Copy content")

                if store.activeArtifact != nil {
                    Button {
                        showClearConfirmation = true
                    } label: {
                        Image(systemName: "trash")
                            .font(.system(size: 13))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .accessibilityLabel("Clear artifact")
                }

                Button {
                    // Close only — does not delete the artifact
                    onDismiss?()
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .accessibilityLabel("Close canvas")
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.sm)
            .background(Design.Colors.surface)

            Divider().background(Design.Colors.border)

            // Editor
            if store.activeArtifact != nil {
                TextEditor(text: $editedContent)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Design.Colors.foreground)
                    .scrollContentBackground(.hidden)
                    .background(Design.Colors.background)
                    .onChange(of: editedContent) { _, newValue in
                        store.updateContent(newValue)
                    }
            } else {
                VStack {
                    Spacer()
                    Text("No artifact open")
                        .font(.system(.caption))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Text("Long-press a message with code and tap \"Open in Canvas\"")
                        .font(.system(.caption2))
                        .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, Design.Spacing.xl)
                    Spacer()
                }
            }
        }
        .background(Design.Colors.background)
        .onAppear {
            editedContent = store.activeArtifact?.content ?? ""
        }
        .onChange(of: store.activeArtifact?.id) { _, _ in
            editedContent = store.activeArtifact?.content ?? ""
        }
        .confirmationDialog(
            "Clear Artifact",
            isPresented: $showClearConfirmation,
            titleVisibility: .visible
        ) {
            Button("Clear", role: .destructive) {
                store.clear()
                onDismiss?()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This will permanently delete the artifact from this session.")
        }
    }
}
