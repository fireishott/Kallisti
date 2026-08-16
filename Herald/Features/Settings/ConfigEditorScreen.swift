import SwiftUI
import UIKit

/// Build 128.41: view and edit the Hermes config.yaml from Settings.
/// Fetches via GET /v1/config (connector facade), validates + saves via
/// PUT /v1/config (connector backs up the live file before writing).
struct ConfigEditorScreen: View {
    @Environment(AppContainer.self) private var container

    @State private var content = ""
    @State private var path = "~/.hermes/config.yaml"
    @State private var isLoading = true
    @State private var isSaving = false
    @State private var errorMessage: String?
    @State private var savedMessage: String?

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

                    ScrollView {
                        TextEditor(text: $content)
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundStyle(Design.Colors.foreground)
                            .scrollContentBackground(.hidden)
                            .frame(minHeight: 500)
                            .padding(Design.Spacing.md)
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
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            Text(path)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(Design.Colors.secondaryForeground)
                .lineLimit(1)
                .truncationMode(.middle)

            if let savedMessage {
                Text(savedMessage)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Design.Colors.success)
            }
            if let errorMessage {
                Text(errorMessage)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Design.Colors.danger)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
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
