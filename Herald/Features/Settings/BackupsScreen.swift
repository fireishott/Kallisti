import SwiftUI

// MARK: - Backups
//
// The daemon's archive list, and the two things a person does with it: take one
// now, or go back to one.
//
// Both actions are destructive in the direction that matters - a create writes a
// file the daemon will keep, a restore replaces the live state - so both are
// confirmed, and the restore's confirmation says the one thing that is easy to
// get wrong: the archive verifies and lands on disk, and the daemon reads it on
// its next start. A screen that said only "restored" would be telling half of it.

struct BackupsScreen: View {
    @Environment(ChatStore.self) private var chatStore

    @State private var archives: [ErisBackup] = []
    @State private var isLoading = false
    @State private var isCreating = false
    @State private var errorMessage: String?
    @State private var notice: String?
    /// The archive a person has asked to go back to, held until they confirm.
    @State private var pendingRestore: ErisBackup?
    @State private var restoringName: String?

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            ScrollView(.vertical) {
                VStack(spacing: Design.Spacing.lg) {
                    if let errorMessage {
                        errorCard(errorMessage)
                    }
                    if let notice {
                        Text(notice)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }

                    SettingsSectionView(title: "Backups") {
                        VStack(spacing: 0) {
                            Button {
                                Task { await create() }
                            } label: {
                                HStack(spacing: Design.Spacing.sm) {
                                    Image(systemName: "plus.circle")
                                        .font(.system(size: 14))
                                        .foregroundStyle(Design.Colors.accent)
                                        .frame(width: 20, alignment: .center)
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(isCreating ? "Writing an archive..." : "Create a backup")
                                            .font(Design.Typography.callout)
                                            .foregroundStyle(Design.Colors.foreground)
                                        Text("Verified before it is kept. The daemon names it.")
                                            .font(Design.Typography.caption)
                                            .foregroundStyle(Design.Colors.secondaryForeground)
                                    }
                                    Spacer()
                                    if isCreating {
                                        ProgressView().tint(Design.Colors.accent)
                                    }
                                }
                                .frame(minHeight: Design.Size.minTapTarget)
                            }
                            .buttonStyle(.plain)
                            .disabled(isCreating || restoringName != nil)
                            .accessibilityIdentifier("backups.create")
                        }
                    }

                    if archives.isEmpty, !isLoading {
                        Text("No archives on the daemon yet.")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    } else if !archives.isEmpty {
                        SettingsSectionView(title: "Archives") {
                            VStack(spacing: 0) {
                                ForEach(Array(archives.enumerated()), id: \.element.id) { index, archive in
                                    if index > 0 { divider }
                                    row(archive)
                                }
                            }
                        }
                    }

                    if isLoading, archives.isEmpty {
                        ProgressView().tint(Design.Colors.accent)
                            .padding(.top, Design.Spacing.lg)
                    }
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.sm)
            }
            .refreshable { await load() }
        }
        .navigationTitle("Backups")
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
        .alert("Go back to this archive?", isPresented: Binding(
            get: { pendingRestore != nil },
            set: { if !$0 { pendingRestore = nil } }
        )) {
            Button("Cancel", role: .cancel) { pendingRestore = nil }
            Button("Restore", role: .destructive) {
                guard let archive = pendingRestore else { return }
                pendingRestore = nil
                Task { await restore(archive) }
            }
        } message: {
            if let archive = pendingRestore {
                Text("\(archive.name) replaces the live state. The daemon keeps a safety copy of what is there now, and the restored state is live on its next start.")
            }
        }
    }

    // MARK: - Rows

    private func row(_ archive: ErisBackup) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: "archivebox")
                .font(.system(size: 14))
                .foregroundStyle(Design.Colors.secondaryForeground)
                .frame(width: 20, alignment: .center)

            VStack(alignment: .leading, spacing: 2) {
                Text(archive.name)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(detail(archive))
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }

            Spacer()

            if restoringName == archive.name {
                ProgressView().tint(Design.Colors.accent)
            } else {
                Button("Restore") { pendingRestore = archive }
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.accent)
                    .disabled(restoringName != nil)
                    .accessibilityIdentifier("backups.restore.\(archive.name)")
            }
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    /// The size, the member count and the date, in the order a person checks them.
    private func detail(_ archive: ErisBackup) -> String {
        var parts = [archive.sizeText, archive.membersText]
        if let date = archive.date {
            parts.append(date.formatted(date: .abbreviated, time: .shortened))
        } else if !archive.createdAt.isEmpty {
            parts.append(archive.createdAt)
        }
        return parts.joined(separator: " · ")
    }

    private func errorCard(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            Text("The daemon refused")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
            Text(message)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
    }

    private var divider: some View {
        Rectangle()
            .fill(Design.Colors.border)
            .frame(height: 1)
            .padding(.vertical, Design.Spacing.xs)
    }

    // MARK: - Wire

    private func erisClient() async -> ErisClient? {
        guard let eris = chatStore.heraldClient as? ErisHeraldClient else { return nil }
        return await eris.client()
    }

    private func load() async {
        isLoading = true
        defer { isLoading = false }
        guard let client = await erisClient() else {
            errorMessage = "No Eris daemon is paired."
            return
        }
        do {
            archives = try await client.backups().archives
            errorMessage = nil
        } catch {
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    private func create() async {
        guard !isCreating else { return }
        guard let client = await erisClient() else { return }
        isCreating = true
        notice = nil
        defer { isCreating = false }
        do {
            let created = try await client.createBackup()
            notice = created.summary
            await load()
        } catch {
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    private func restore(_ archive: ErisBackup) async {
        guard let client = await erisClient() else { return }
        restoringName = archive.name
        notice = nil
        defer { restoringName = nil }
        do {
            let result = try await client.restoreBackup(name: archive.name)
            // The daemon verified every member and put them in place, but it
            // still holds the live database open: the restored state is what the
            // next start reads. Saying only "restored" would be half of it.
            notice = result.restored
                ? "\(result.name) verified and restored. Restart the daemon to run on it."
                : "\(result.name) was not restored."
            await load()
        } catch {
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }
}
