import SwiftUI

/// Build 135.40: skill detail view. Shows metadata + path. SKILL.md content
/// editing from iOS needs a gateway content endpoint (skills.manage inspect
/// returns hub metadata, not local SKILL.md); the Cron editor is the fully
/// wired rich editor this build.
struct SkillDetailView: View {
    let skill: SkillsStore.HeraldSkill

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if !skill.description.isEmpty {
                    Text(skill.description)
                        .font(.body)
                }
                Divider()
                Label(skill.path, systemImage: "doc.text")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .textSelection(.enabled)
            }
            .padding()
        }
        .navigationTitle(skill.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}
