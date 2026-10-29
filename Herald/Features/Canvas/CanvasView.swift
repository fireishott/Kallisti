import SwiftUI

/// Editable canvas panel for AI-generated code artifacts.
/// Shown as a sheet on iPhone, tab on iPad.
/// Close (X) dismisses without deleting. Clear/Delete removes the artifact.
///
/// Build 78: when a tool/subagent is actively running (the streaming
/// message has non-empty tool activities), the canvas shows a tab
/// picker with two tabs — "Artifact" (existing editor / empty state)
/// and "Live" (real-time tool activity list). The Live tab is
/// auto-selected the first time activities appear.
struct CanvasView: View {
    @Bindable var store: HeraldCanvasStore
    var onDismiss: (() -> Void)? = nil
    /// Subagents the daemon is running. Read here so the sheet and the toolbar
    /// badge are the same numbers, from the same store.
    @Environment(SubagentStore.self) private var subagents

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
                } else if store.isLiveActivityVisible || store.isProcessFeedVisible {
                    Label("Live", systemImage: "bolt.horizontal.fill")
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

            // Build 78: tab picker — only shown when there are live
            // tool activities to display (or background processes).
            if store.isLiveActivityVisible || store.isProcessFeedVisible || subagents.hasRows {
                Picker(
                    "",
                    selection: Binding(
                        get: { store.activeTab },
                        set: { store.activeTab = $0 }
                    )
                ) {
                    Text("Artifact").tag(HeraldCanvasStore.CanvasTab.artifact)
                    Text("Live").tag(HeraldCanvasStore.CanvasTab.live)
                    Text(subagents.activeCount > 0
                        ? "Subagents (\(subagents.activeCount))"
                        : "Subagents")
                        .tag(HeraldCanvasStore.CanvasTab.subagents)
                }
                .pickerStyle(.segmented)
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.sm)
                .background(Design.Colors.surface)

                Divider().background(Design.Colors.border)
            }

            // Content area
            if store.activeTab == .subagents {
                subagentList
            } else if (store.isLiveActivityVisible || store.isProcessFeedVisible) && store.activeTab == .live {
                liveActivityList
            } else if store.activeArtifact != nil {
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
        // Build 78: auto-select Live the first time activities appear.
        .onChange(of: store.isLiveActivityVisible) { _, isVisible in
            if isVisible && store.activeTab == .artifact {
                store.activeTab = .live
            }
        }
        // Build 135.17: also auto-select Live when background processes
        // show up, so the user sees the running task without hunting.
        .onChange(of: store.isProcessFeedVisible) { _, isVisible in
            if isVisible && store.activeTab == .artifact {
                store.activeTab = .live
            }
        }
        // A child starting is the moment this sheet is worth opening, so the
        // Subagents tab selects itself the first time one is running.
        .onChange(of: subagents.activeCount) { _, count in
            if count > 0 && store.activeTab == .artifact {
                store.activeTab = .subagents
            }
        }
        .task { await subagents.refresh() }
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

    /// The subagents the daemon is running, and the ones it recently finished.
    ///
    /// Same rows the dashboard bar draws, read from `/v1/subagents` on a poll, so
    /// a child that starts while this sheet is open appears without a reload and
    /// settles in place when it finishes.
    private var subagentList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Design.Spacing.sm) {
                if subagents.rows.isEmpty {
                    VStack(spacing: Design.Spacing.xs) {
                        Text("No subagents")
                            .font(.system(.caption))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Text("Ask for work to be handed off and it appears here while it runs.")
                            .font(.system(.caption2))
                            .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.top, Design.Spacing.xl)
                } else {
                    ForEach(subagents.rows) { row in
                        subagentRow(row)
                    }
                    if subagents.truncated {
                        Text("Older subagents are not shown here.")
                            .font(.system(.caption2))
                            .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
                    }
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.md)
        }
        .background(Design.Colors.background)
    }

    /// One subagent: label, status, model, elapsed, and its result once there is
    /// one. The status word is the daemon's own, never a client paraphrase.
    @ViewBuilder
    private func subagentRow(_ row: ErisSubagent) -> some View {
        let accent: Color = row.status == "failed"
            ? .orange
            : (row.isActive ? Design.Brand.accent : .green)
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: Design.Spacing.xs) {
                Circle()
                    .fill(accent)
                    .frame(width: 7, height: 7)
                Text(row.label)
                    .font(.system(.caption, weight: .semibold))
                    .foregroundStyle(Design.Colors.foreground)
                Spacer()
                Text(row.status.uppercased())
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(accent)
                Text(row.elapsed)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            Text(row.prompt)
                .font(.system(.caption2))
                .foregroundStyle(Design.Colors.secondaryForeground)
                .lineLimit(2)
            if let result = row.result, !result.isEmpty {
                Text(result)
                    .font(.system(.footnote))
                    .foregroundStyle(Design.Colors.foreground)
                    .lineLimit(6)
                    .padding(.top, 2)
            } else if let code = row.errorCode {
                Text(code)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.orange)
            }
            Text(row.model)
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
        }
        .padding(Design.Spacing.sm)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Design.Colors.surface.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                .stroke(Design.Colors.divider, lineWidth: 1)
        )
    }

    /// Build 78: scrollable list of in-flight and completed tool
    /// activities. Each row shows the tool name, duration / running
    /// status, a status indicator, and a 2-line result preview.
    /// Build 135.17: background process rows render below the tool
    /// activities, each with its live output tail and a Stop button
    /// while running.
    private var liveActivityList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Design.Spacing.sm) {
                ForEach(store.liveToolActivities) { activity in
                    liveActivityRow(activity)
                }
                if !store.liveBackgroundProcesses.isEmpty {
                    Divider().background(Design.Colors.border)
                    Label("Background tasks", systemImage: "terminal.fill")
                        .font(.system(.caption, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .padding(.top, Design.Spacing.xs)
                    ForEach(store.liveBackgroundProcesses) { process in
                        backgroundProcessRow(process)
                    }
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.md)
        }
        .background(Design.Colors.background)
    }

    /// Build 135.17: one tracked background process. Running rows get a
    /// spinner + Stop button; terminal rows show exit status + tail.
    @ViewBuilder
    private func backgroundProcessRow(_ process: BackgroundProcess) -> some View {
        let statusColor: Color = process.status == .failed
            ? .orange
            : (process.status.isActive ? Design.Brand.accent : .green)
        let statusSymbol: String = process.status == .failed
            ? "exclamationmark.triangle.fill"
            : (process.status.isActive ? "circle.dotted" : "checkmark.circle.fill")
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.sm) {
                if process.status.isActive {
                    ProgressView()
                        .controlSize(.small)
                        .tint(statusColor)
                } else {
                    Image(systemName: statusSymbol)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(statusColor)
                }
                Text(process.name.isEmpty ? process.commandLine : process.name)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Design.Colors.foreground)
                    .lineLimit(1)
                Spacer()
                if let code = process.exitCode, !process.status.isActive {
                    Text("exit \(code)")
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(process.status == .failed ? .orange : Design.Colors.secondaryForeground)
                }
                if process.status.isActive {
                    Button {
                        // Build 135.17: best-effort kill via the store's
                        // callback, wired by AppContainer to the connector's
                        // /v1/canvas/processes/{id}/kill endpoint.
                        store.killBackgroundProcess?(process.id)
                    } label: {
                        Image(systemName: "stop.circle")
                            .font(.system(size: 15))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Stop \(process.name)")
                }
            }
            if !(process.outputTail ?? "").isEmpty {
                TerminalOutputView(
                    text: process.outputTail ?? "",
                    isActive: process.status.isActive,
                    maxChars: 24 * 1024
                )
            }
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Design.Colors.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
    }

    @ViewBuilder
    private func liveActivityRow(_ activity: ToolActivity) -> some View {
        let statusColor: Color = activity.isError
            ? .orange
            : (!activity.isActive ? .green : Design.Brand.accent)
        let statusSymbol: String = activity.isError
            ? "exclamationmark.triangle.fill"
            : (!activity.isActive ? "checkmark.circle.fill" : "circle.dotted")
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.sm) {
                if activity.isActive {
                    ProgressView()
                        .controlSize(.small)
                        .tint(statusColor)
                } else {
                    Image(systemName: statusSymbol)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(statusColor)
                }
                Text(activity.name ?? activity.label)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Design.Colors.foreground)
                    .lineLimit(1)
                Spacer()
                Text(liveActivityDurationLabel(activity))
                    .font(.system(.caption2, design: .monospaced))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            if let preview = activity.resultPreview, !preview.isEmpty, activity.liveOutput.isEmpty {
                Text(preview)
                    .font(.system(.caption2))
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .lineLimit(2)
                    .truncationMode(.tail)
            }

            // Build 128.56: live stdout renders through the real terminal view
            // so the Live tab reads like a terminal monitor, not a JSON dump.
            if !activity.liveOutput.isEmpty {
                TerminalOutputView(
                    text: activity.liveOutput,
                    isActive: activity.isActive,
                    maxChars: 48 * 1024
                )
            }
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Design.Colors.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
    }

    /// Build 78: human-readable duration for a tool activity row.
    /// "running..." while active, else elapsed since start.
    private func liveActivityDurationLabel(_ activity: ToolActivity) -> String {
        if activity.isActive { return "running..." }
        if let ms = activity.durationMs { return formatDurationMs(ms) }
        let end = activity.finishedAt ?? Date()
        let elapsed = end.timeIntervalSince(activity.startedAt)
        return formatDurationMs(Int(elapsed * 1000))
    }

    private func formatDurationMs(_ ms: Int) -> String {
        if ms < 1000 { return "\(ms)ms" }
        let s = Double(ms) / 1000.0
        if s < 60 { return String(format: "%.1fs", s) }
        let m = Int(s / 60)
        let r = Int(s.truncatingRemainder(dividingBy: 60))
        return "\(m)m\(r)s"
    }
}
