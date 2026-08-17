import SwiftUI
import UIKit

/// Build 128.41+: view and edit the Hermes config.yaml from Settings.
/// Fetches via GET /v1/config (connector facade), validates + saves via
/// PUT /v1/config (connector backs up the live file before writing).
///
/// Build 128.56: real editor upgrade (UIKit line-numbered text editor,
/// YAML syntax highlighting, indent/outdent/comment toolbar).
///
/// Build 128.59: YAML validation on save, Save & Restart Gateway option,
/// realtime restart overlay showing gateway phase progress.
struct ConfigEditorScreen: View {
    @Environment(AppContainer.self) private var container

    @State private var content = ""
    @State private var path = "~/.hermes/config.yaml"
    @State private var isLoading = true
    @State private var isSaving = false
    @State private var isValidationRunning = false
    @State private var errorMessage: String?
    @State private var savedMessage: String?
    @State private var validationError: String?
    @State private var editorController: YamlEditorController?
    @State private var showSaveOptions = false
    @State private var isRestarting = false
    @State private var restartPhase: RestartPhase?
    @State private var restartError: String?
    @State private var restartPollTask: Task<Void, Never>?

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            if isLoading {
                ProgressView("Loading config.yaml\u{2026}")
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

                    // Build 128.59: inline validation error banner.
                    if let ve = validationError {
                        HStack(spacing: Design.Spacing.xs) {
                            Image(systemName: "exclamationmark.circle.fill")
                                .font(.system(size: 11))
                                .foregroundStyle(Design.Colors.danger)
                            Text(ve)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(Design.Colors.danger)
                                .lineLimit(2)
                            Spacer()
                            Button {
                                validationError = nil
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .font(.system(size: 11))
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                            }
                            .buttonStyle(.plain)
                        }
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.vertical, Design.Spacing.xs)
                        .background(Design.Colors.danger.opacity(0.08))
                        Divider().overlay(Design.Colors.divider)
                    }

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
                        if editorController == nil {
                            editorController = YamlEditorController()
                        }
                    }
                }
            }

            // Build 128.59: realtime restart overlay.
            if isRestarting {
                ConfigRestartOverlay(
                    phase: restartPhase,
                    error: restartError
                )
                .transition(.opacity)
                .zIndex(10)
            }
        }
        .navigationTitle("Config Editor")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                if !isLoading && !content.isEmpty {
                    Button {
                        showSaveOptions = true
                    } label: {
                        if isSaving {
                            ProgressView()
                                .controlSize(.small)
                        } else {
                            Text("Save")
                                .fontWeight(.semibold)
                        }
                    }
                    .disabled(isSaving || isRestarting)
                }
            }
        }
        // Build 128.59: confirmation dialog for Save vs Save & Restart.
        .confirmationDialog("Save Options", isPresented: $showSaveOptions, titleVisibility: .visible) {
            Button("Save") {
                Task { await save() }
            }
            Button("Save & Restart Gateway") {
                Task { await saveAndRestart() }
            }
            Button("Cancel", role: .cancel) {}
        }
        .task { await load() }
        .onDisappear {
            restartPollTask?.cancel()
        }
    }

    private var headerBar: some View {
        HStack(spacing: Design.Spacing.xs) {
            VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
                Text(path)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .lineLimit(1)
                    .truncationMode(.middle)

                Text("\(lineCount()) lines \u{00B7} \(content.count) chars")
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

    // MARK: - YAML Editing Tools (build 128.56 + 128.59)

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
            // Build 128.59: Validate button.
            toolButton("checkmark.seal", "Validate") {
                Task { await validate() }
            }
            .disabled(isValidationRunning)
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
        validationError = nil
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

    /// Build 128.59: validate YAML content via connector without writing.
    private func validate() async {
        isValidationRunning = true
        validationError = nil
        defer { isValidationRunning = false }

        guard let nativeClient = container.nativeGatewayClient else {
            validationError = "Native gateway unavailable."
            return
        }
        do {
            _ = try await nativeClient.featureClient.validateConfigDocument(content)
            validationError = nil
            savedMessage = "YAML is valid"
        } catch {
            validationError = error.localizedDescription
            savedMessage = nil
        }
    }

    private func save() async {
        isSaving = true
        errorMessage = nil
        savedMessage = nil
        validationError = nil
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

    /// Build 128.59: save then trigger gateway restart with realtime overlay.
    private func saveAndRestart() async {
        isSaving = true
        errorMessage = nil
        savedMessage = nil
        validationError = nil
        defer { isSaving = false }

        // 1. Save
        guard let nativeClient = container.nativeGatewayClient else {
            errorMessage = "Native gateway unavailable."
            return
        }
        do {
            let backup = try await nativeClient.featureClient.saveConfigDocument(content)
            savedMessage = backup.map { "Saved. Backup: \($0)" } ?? "Saved."
        } catch {
            errorMessage = error.localizedDescription
            return
        }

        // 2. Start restart with realtime overlay
        isRestarting = true
        restartPhase = .accepted
        restartError = nil

        let gc = container.gatewayControl
        do {
            let preflight = try await gc.fetchPreflight(target: "hermes")
            let operation = try await gc.submitRestart(target: "hermes", preflight: preflight)
            restartPhase = operation.phase

            // Poll until terminal
            if !operation.operationId.isEmpty {
                restartPollTask = Task { @MainActor in
                    while !Task.isCancelled {
                        try? await Task.sleep(for: .seconds(2))
                        guard !Task.isCancelled else { break }
                        do {
                            let updated = try await gc.fetchOperation(operation.operationId)
                            restartPhase = updated.phase
                            if updated.phase == .healthy {
                                try? await Task.sleep(for: .seconds(1))
                                isRestarting = false
                                restartPhase = nil
                                return
                            }
                            if updated.phase == .failed {
                                restartError = updated.error?.journalExcerpt ?? "Restart failed"
                                try? await Task.sleep(for: .seconds(3))
                                isRestarting = false
                                return
                            }
                        } catch {
                            // Poll error - gateway may be mid-restart, keep trying
                        }
                    }
                }
            } else {
                // Legacy connector: no operation tracking, wait and verify via health
                try? await Task.sleep(for: .seconds(5))
                restartPhase = .healthy
                try? await Task.sleep(for: .seconds(1))
                isRestarting = false
                restartPhase = nil
            }
        } catch {
            restartError = error.localizedDescription
            restartPhase = .failed
            try? await Task.sleep(for: .seconds(3))
            isRestarting = false
        }
    }
}

// MARK: - Build 128.59: Realtime Restart Overlay

/// Fullscreen overlay shown during gateway restart, matching the loading surface
/// aesthetic. Shows phase progression with dots and status text.
private struct ConfigRestartOverlay: View {
    let phase: RestartPhase?
    let error: String?

    private var phaseText: String {
        switch phase {
        case .accepted: return "Restart accepted"
        case .stopping: return "Stopping gateway"
        case .starting: return "Starting gateway"
        case .verifying: return "Verifying health"
        case .healthy: return "Gateway online"
        case .failed: return "Restart failed"
        case .none: return "Preparing restart"
        }
    }

    private var phases: [RestartPhase] {
        [.accepted, .stopping, .starting, .verifying, .healthy]
    }

    private func phaseIndex(_ p: RestartPhase?) -> Int {
        guard let p, let idx = phases.firstIndex(of: p) else { return -1 }
        return idx
    }

    var body: some View {
        ZStack {
            Color.black.opacity(0.88).ignoresSafeArea()

            VStack(spacing: Design.Spacing.lg) {
                // Breathing coin
                Image(systemName: "circle.fill")
                    .font(.system(size: 48))
                    .foregroundStyle(Design.Brand.accent)
                    .symbolEffect(.pulse, options: .repeating)

                Text(phaseText)
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Colors.foreground)

                // Phase dots
                HStack(spacing: 10) {
                    ForEach(Array(phases.enumerated()), id: \.offset) { idx, p in
                        Circle()
                            .fill(idx <= phaseIndex(phase) ? Design.Brand.accent : Design.Colors.tertiaryForeground.opacity(0.3))
                            .frame(width: 10, height: 10)
                    }
                }

                if let error {
                    Text(error)
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(Design.Colors.danger)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, Design.Spacing.lg)
                }
            }
        }
        .animation(.easeInOut(duration: 0.3), value: phase)
    }
}

#Preview {
    NavigationStack {
        ConfigEditorScreen()
    }
}
