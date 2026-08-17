import SwiftUI
import UIKit

/// Build 128.41+: view and edit the Hermes config.yaml from Settings.
/// Fetches via GET /v1/config (connector facade), validates + saves via
/// PUT /v1/config (connector backs up the live file before writing).
///
/// Build 128.56: real editor upgrade. The plain TextEditor is replaced by a
/// UIKit line-numbered text editor with YAML syntax highlighting and editing
/// tools (indent / outdent / comment toggle). Font bumped to 13pt mono for
/// readability.
struct ConfigEditorScreen: View {
    @Environment(AppContainer.self) private var container

    @State private var content = ""
    @State private var path = "~/.hermes/config.yaml"
    @State private var isLoading = true
    @State private var isSaving = false
    @State private var errorMessage: String?
    @State private var savedMessage: String?
    @State private var editorController: YamlEditorController?

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            if isLoading {
                ProgressView("Loading config.yaml…")
                    .foregroundStyle(Design.Colors.secondaryForeground)
            } else if let error = errorMessage, content.isEmpty {
                VStack(spacing: Design.Spacing.md) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 40))
                        .foregroundStyle(Design.Colors.warning)
                    Text("Unable to load config")
                        .font(Design.Typography.headline)
                        .foregroundStyle(Design.Colors.foreground)
                    Text(error)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .multilineTextAlignment(.center)
                    Button("Retry") {
                        Task { await load() }
                    }
                    .buttonStyle(.borderedProminent)
                }
                .padding()
            } else {
                VStack(spacing: 0) {
                    headerBar

                    Divider().overlay(Design.Colors.divider)

                    // Build 128.56: YAML editing tools.
                    editorToolbar
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.vertical, Design.Spacing.xs)

                    Divider().overlay(Design.Colors.divider)

                    YamlEditorView(
                        text: $content,
                        controller: editorController
                    )
                    .onAppear {
                        // Create the shared controller once so the toolbar can
                        // drive indent/outdent/comment actions on the editor.
                        if editorController == nil {
                            editorController = YamlEditorController()
                        }
                    }
                }
            }
        }
        .navigationTitle("Config Editor")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                if !isLoading && !content.isEmpty {
                    Button {
                        Task { await save() }
                    } label: {
                        if isSaving {
                            ProgressView()
                                .controlSize(.small)
                        } else {
                            Text("Save")
                                .fontWeight(.semibold)
                        }
                    }
                    .disabled(isSaving)
                }
            }
        }
        .task { await load() }
    }

    private var headerBar: some View {
        HStack(spacing: Design.Spacing.xs) {
            VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
                Text(path)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .lineLimit(1)
                    .truncationMode(.middle)

                // Build 128.56: line + char count for context.
                Text("\(lineCount()) lines · \(content.count) chars")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }

            Spacer()

            if let savedMessage {
                Text(savedMessage)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Design.Colors.success)
                    .lineLimit(1)
            }
            if let errorMessage {
                Text(errorMessage)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Design.Colors.danger)
                    .lineLimit(2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
    }

    // MARK: - YAML Editing Tools (build 128.56)

    private var editorToolbar: some View {
        HStack(spacing: Design.Spacing.sm) {
            toolButton("increase.indent", "Indent") {
                editorController?.indentSelection()
            }
            toolButton("decrease.indent", "Outdent") {
                editorController?.outdentSelection()
            }
            toolButton("number", "Comment") {
                editorController?.toggleCommentSelection()
            }
            Spacer()
            Text("YAML")
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .tracking(0.8)
                .foregroundStyle(Design.Colors.tertiaryForeground)
        }
    }

    private func toolButton(_ icon: String, _ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 2) {
                Image(systemName: icon)
                    .font(.system(size: 14, weight: .medium))
                Text(label)
                    .font(.system(size: 9, weight: .medium, design: .monospaced))
            }
            .foregroundStyle(Design.Brand.accent)
            .frame(minWidth: 52, minHeight: 36)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    private func lineCount() -> Int {
        guard !content.isEmpty else { return 1 }
        return content.split(separator: "\n", omittingEmptySubsequences: false).count
    }

    // MARK: - Actions

    private func load() async {
        isLoading = true
        errorMessage = nil
        savedMessage = nil
        defer { isLoading = false }

        guard let nativeClient = container.nativeGatewayClient else {
            errorMessage = "Native gateway unavailable. Check that Kallisti is connected."
            return
        }
        let doc = await nativeClient.featureClient.configDocument()
        if let doc {
            content = doc.content
            path = doc.path
        } else {
            errorMessage = "Couldn't fetch config.yaml from the host."
        }
    }

    private func save() async {
        isSaving = true
        errorMessage = nil
        savedMessage = nil
        defer { isSaving = false }

        guard let nativeClient = container.nativeGatewayClient else {
            errorMessage = "Native gateway unavailable. Check that Kallisti is connected."
            return
        }
        do {
            let backup = try await nativeClient.featureClient.saveConfigDocument(content)
            savedMessage = backup.map { "Saved. Backup: \($0)" } ?? "Saved."
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

#Preview {
    NavigationStack {
        ConfigEditorScreen()
    }
}