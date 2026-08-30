import SwiftUI

struct SkillsBrowserView: View {
    @Environment(SkillsStore.self) private var skillsStore

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
    }
}
