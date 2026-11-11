import SwiftUI

// MARK: - Memory
//
// The two files the assistant writes for itself: what it decided to keep about its
// own working habits, and what it knows about the operator. Both are capped, both
// are plain text on the daemon, and both are quoted into every future prompt.
//
// Read-only on purpose. Writing is the `memory_manage` tool inside a turn, because
// every write has to pass the daemon's injection and credential scan, and a screen
// that wrote directly would be a second path around it.

struct MemoryScreen: View {
    @Environment(ChatStore.self) private var chatStore

    @State private var files: [ErisMemoryFile] = []
    @State private var isLoading = false
    @State private var errorMessage: String?

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

                    Text("The assistant keeps two short notes for itself. They survive every session and ride at the top of the next one, which is why they are capped: a note that grows without bound is a note that crowds out the work.")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, Design.Spacing.lg)

                    if files.isEmpty && !isLoading {
                        Text("Both files are empty. Nothing has been written yet.")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, Design.Spacing.lg)
                    } else {
                        ForEach(files) { file in
                            fileCard(file)
                        }
                    }
                }
                .padding(.vertical, Design.Spacing.lg)
            }
        }
        .navigationTitle("Memory")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await load() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .disabled(isLoading)
                .accessibilityIdentifier("memory.refresh")
            }
        }
        .task { await load() }
        .refreshable { await load() }
    }

    // MARK: - Pieces

    @ViewBuilder
    private func fileCard(_ file: ErisMemoryFile) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            HStack(spacing: Design.Spacing.sm) {
                Text(file.title)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
                Spacer(minLength: 0)
                Text("\(file.used) / \(file.limit)")
                    .font(Design.Typography.caption.monospaced())
                    .foregroundStyle(
                        file.remaining < file.limit / 10
                            ? Design.Colors.danger
                            : Design.Colors.secondaryForeground
                    )
            }

            Text(file.file)
                .font(Design.Typography.caption.monospaced())
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))

            if file.entries.isEmpty {
                Text("Empty.")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            } else {
                ForEach(Array(file.entries.enumerated()), id: \.offset) { _, entry in
                    HStack(alignment: .top, spacing: Design.Spacing.xs) {
                        Text("•")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.accent)
                        Text(entry)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.foreground)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Design.Spacing.lg)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md, style: .continuous))
        .padding(.horizontal, Design.Spacing.lg)
        .accessibilityIdentifier("memory.file.\(file.target)")
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
            files = try await client.memory().files
            errorMessage = nil
        } catch {
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }
}
