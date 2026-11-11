import SwiftUI

// MARK: - Processes
//
// Work the daemon is doing in the background. Before 1.6.0 a long command became a
// `nohup` shell the app could not see, poll or stop; the daemon now records every
// one as a jobs row of kind `tool` and keeps a handle on it.
//
// This screen is the hand on that table. It shows what is running, what finished,
// and how each one ended - including the signal sentence, because "exit -9" is not
// an explanation and the daemon already knows the explanation.

struct ProcessesScreen: View {
    @Environment(ChatStore.self) private var chatStore

    @State private var rows: [ErisProcessRow] = []
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var killing: String?
    @State private var pendingKill: ErisProcessRow?

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            ScrollView(.vertical) {
                VStack(spacing: Design.Spacing.lg) {
                    if let errorMessage {
                        Text(errorMessage)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.danger)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, Design.Spacing.lg)
                    }

                    if rows.isEmpty && !isLoading {
                        Text("Nothing is running in the background. A command a turn starts with the background flag appears here until it finishes.")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, Design.Spacing.lg)
                    } else {
                        VStack(spacing: 0) {
                            ForEach(rows) { row in
                                processRow(row)
                                Rectangle()
                                    .fill(Design.Colors.border)
                                    .frame(height: 1)
                            }
                        }
                    }
                }
                .padding(.vertical, Design.Spacing.lg)
            }
        }
        .navigationTitle("Processes")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await load() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .disabled(isLoading)
                .accessibilityIdentifier("processes.refresh")
            }
        }
        .task { await load() }
        .refreshable { await load() }
        .confirmationDialog(
            "Stop this process?",
            isPresented: Binding(
                get: { pendingKill != nil },
                set: { if !$0 { pendingKill = nil } }
            ),
            presenting: pendingKill
        ) { row in
            Button("Stop \(row.processId)", role: .destructive) {
                Task { await kill(row) }
            }
            Button("Leave it running", role: .cancel) { pendingKill = nil }
        } message: { row in
            Text("The daemon asks with SIGTERM, waits five seconds, then kills it. The command was: \(row.command)")
        }
    }

    // MARK: - Pieces

    @ViewBuilder
    private func processRow(_ row: ErisProcessRow) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.sm) {
                Circle()
                    .fill(row.isRunning ? Design.Colors.accent : Design.Colors.secondaryForeground.opacity(0.4))
                    .frame(width: 8, height: 8)
                Text(row.processId)
                    .font(Design.Typography.caption.monospaced())
                    .foregroundStyle(Design.Colors.foreground)
                Spacer(minLength: 0)
                Text(row.isRunning ? "running" : row.status)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                if row.isRunning {
                    Button {
                        pendingKill = row
                    } label: {
                        Image(systemName: "stop.circle")
                            .font(.system(size: 16))
                            .foregroundStyle(Design.Colors.danger)
                    }
                    .buttonStyle(.plain)
                    .disabled(killing != nil)
                    .accessibilityIdentifier("processes.kill.\(row.processId)")
                }
            }
            Text(row.command)
                .font(Design.Typography.caption.monospaced())
                .foregroundStyle(Design.Colors.foreground)
                .lineLimit(2)
            HStack(spacing: Design.Spacing.sm) {
                Text(row.startedAt)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                if let bytes = row.bytes, bytes > 0 {
                    Text(byteCount(bytes))
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                }
                if killing == row.processId {
                    ProgressView().tint(Design.Colors.accent).scaleEffect(0.7)
                }
            }
            if let note = row.signalNote {
                Text(note)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.danger)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Design.Spacing.lg)
        .padding(.vertical, Design.Spacing.sm)
        .accessibilityIdentifier("processes.row.\(row.processId)")
    }

    private func byteCount(_ bytes: Int) -> String {
        if bytes >= 1_048_576 {
            return String(format: "%.1f MB", Double(bytes) / 1_048_576)
        }
        if bytes >= 1024 {
            return String(format: "%.1f KB", Double(bytes) / 1024)
        }
        return "\(bytes) B"
    }

    // MARK: - Wire

    private func erisClient() async -> ErisClient? {
        guard let eris = chatStore.heraldClient as? ErisHeraldClient else { return nil }
        return await eris.client()
    }

    private func load() async {
        guard let client = await erisClient() else {
            errorMessage = "No Eris daemon is paired."
            return
        }
        isLoading = true
        defer { isLoading = false }
        do {
            rows = try await client.processes().processes
            errorMessage = nil
        } catch {
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    private func kill(_ row: ErisProcessRow) async {
        pendingKill = nil
        guard let client = await erisClient() else { return }
        killing = row.processId
        defer { killing = nil }
        do {
            _ = try await client.killProcess(id: row.processId)
            await load()
        } catch {
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }
}
