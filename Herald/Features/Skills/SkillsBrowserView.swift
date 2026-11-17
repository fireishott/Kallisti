import SwiftUI

struct SkillsBrowserView: View {
    @Environment(SkillsStore.self) private var skillsStore

    var body: some View {
        List {
            if let error = skillsStore.errorMessage, skillsStore.skills.isEmpty {
                Section {
                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        Label("The daemon refused this", systemImage: "lock.slash")
                            .font(Design.Typography.caption.weight(.semibold))
                            .foregroundStyle(Design.Colors.warning)
                        Text(error)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    Button("Try again") {
                        Task { await skillsStore.loadSkills(force: true) }
                    }
                    .buttonStyle(.bordered)
                }
            }
            if skillsStore.isLoading && skillsStore.skills.isEmpty {
                ProgressView("Loading skills...")
            } else if skillsStore.filteredSkills.isEmpty {
                ContentUnavailableView(
                    "No Skills",
                    systemImage: "wrench.and.screwdriver",
                    description: Text(skillsStore.searchText.isEmpty
                        ? "No skills are installed."
                        : "No skills match '\(skillsStore.searchText)'.")
                )
            } else {
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
        }
        .searchable(text: Bindable(skillsStore).searchText, prompt: "Search skills")
        .navigationTitle("Skills")
        .navigationDestination(for: SkillsStore.HeraldSkill.self) { skill in
            SkillDetailView(skill: skill)
        }
        .refreshable { await skillsStore.loadSkills(force: true) }
        .task { await skillsStore.loadSkills() }
    }
}
