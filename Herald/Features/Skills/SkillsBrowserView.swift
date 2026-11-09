import SwiftUI

struct SkillsBrowserView: View {
    @Environment(SkillsStore.self) private var skillsStore

    /// The row a confirmation is currently about. Held so the dialog can name the
    /// skill and the delete can be run against the row it was raised for.
    @State private var pendingDelete: SkillsStore.HeraldSkill?
    @State private var showDeleteConfirmation = false
    @State private var deleteError: String?
    @State private var showDeleteError = false

    var body: some View {
        Group {
            if skillsStore.isLoading && skillsStore.skills.isEmpty {
                ProgressView("Loading skills...")
            } else if let error = skillsStore.errorMessage, skillsStore.skills.isEmpty {
                // Build 135.40: explicit error state instead of the bare
                // yellow-triangle empty state. Shows what failed + Retry.
                ContentUnavailableView {
                    Label("Unable to load skills", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Retry") {
                        Task { await skillsStore.loadSkills(force: true) }
                    }
                    .buttonStyle(.borderedProminent)
                }
            } else if skillsStore.filteredSkills.isEmpty {
                ContentUnavailableView(
                    "No Skills",
                    systemImage: "wrench.and.screwdriver",
                    description: Text(skillsStore.searchText.isEmpty
                        ? "No skills are installed."
                        : "No skills match '\(skillsStore.searchText)'.")
                )
            } else {
                List {
                    ForEach(skillsStore.filteredSkills) { skill in
                        NavigationLink(value: skill) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(skill.name)
                                    .font(.headline)
                                if !skill.description.isEmpty {
                                    Text(skill.description)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(2)
                                }
                                if let provenance = provenance(skill) {
                                    Text(provenance)
                                        .font(.caption2)
                                        .foregroundStyle(.tertiary)
                                }
                            }
                        }
                        .swipeActions(edge: .trailing) {
                            // Only a transport that can delete, and only a skill
                            // the daemon called editable. Everywhere else the row
                            // has no swipe at all rather than one that fails.
                            if skillsStore.canDeleteSkills && skill.editable {
                                Button(role: .destructive) {
                                    pendingDelete = skill
                                    showDeleteConfirmation = true
                                } label: {
                                    Label("Delete", systemImage: "trash")
                                }
                            }
                        }
                    }
                }
                .searchable(text: Bindable(skillsStore).searchText, prompt: "Search skills")
            }
        }
        .navigationTitle("Skills")
        .navigationDestination(for: SkillsStore.HeraldSkill.self) { skill in
            SkillDetailView(skill: skill)
        }
        .refreshable { await skillsStore.loadSkills(force: true) }
        .task { await skillsStore.loadSkills() }
        // The wording is the contract: the daemon destroys the skill directory
        // and drops its learning ledger row. Nothing is kept, so nothing here
        // may promise a way back.
        .confirmationDialog(
            "Delete \(pendingDelete?.name ?? "skill")?",
            isPresented: $showDeleteConfirmation,
            titleVisibility: .visible,
            presenting: pendingDelete
        ) { skill in
            Button("Delete", role: .destructive) {
                Task { await delete(skill) }
            }
            Button("Cancel", role: .cancel) {
                pendingDelete = nil
            }
        } message: { skill in
            Text("'\(skill.name)' is deleted from the daemon. This cannot be undone.")
        }
        .alert("Could not delete the skill", isPresented: $showDeleteError) {
            Button("OK", role: .cancel) { deleteError = nil }
        } message: {
            Text(deleteError ?? "")
        }
    }

    /// Where the skill came from, and where the curator has put it, as one line.
    /// Nil when the daemon said neither, so nothing is claimed on its behalf.
    private func provenance(_ skill: SkillsStore.HeraldSkill) -> String? {
        var parts: [String] = []
        if let origin = skill.originLabel { parts.append(origin) }
        if let state = skill.stateLabel { parts.append(state) }
        return parts.isEmpty ? nil : parts.joined(separator: " / ")
    }

    /// Delete one skill and let the store drop the row.
    ///
    /// A refusal is shown rather than swallowed: the row stays, and the sentence
    /// is the daemon's own.
    private func delete(_ skill: SkillsStore.HeraldSkill) async {
        do {
            try await skillsStore.deleteSkill(key: skill.key)
            pendingDelete = nil
        } catch {
            deleteError = SkillsStore.refusal(error)
            showDeleteError = true
        }
    }
}
