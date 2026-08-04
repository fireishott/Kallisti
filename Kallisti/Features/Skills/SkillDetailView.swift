import SwiftUI

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
