import SwiftUI

/// Build 135.41: skill detail view. Loads the real SKILL.md content from the
/// connector's /v1/skills/{name} endpoint and renders it as scrollable text.
/// Falls back to the list metadata (description + path) when the detail fetch
/// fails, so the screen is never a bare shell.
struct SkillDetailView: View {
    let skill: SkillsStore.HeraldSkill
    @Environment(SkillsStore.self) private var skillsStore

    @State private var detail: SkillsStore.SkillDetail?
    @State private var isLoading = true
    @State private var loadError: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if isLoading {
                    HStack {
                        Spacer()
                        ProgressView("Loading skill...")
                        Spacer()
                    }
                    .padding(.top, 40)
                } else if let loadError {
                    ContentUnavailableView {
                        Label("Unable to load skill", systemImage: "exclamationmark.triangle")
                    } description: {
                        Text(loadError)
                    } actions: {
                        Button("Retry") {
                            Task { await load() }
                        }
                        .buttonStyle(.borderedProminent)
                    }
                } else {
                    let description = detail?.description ?? skill.description
                    if !description.isEmpty {
                        Text(description)
                            .font(.body)
                    }
                    let path = detail?.path ?? skill.path
                    if !path.isEmpty {
                        Label(path, systemImage: "doc.text")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .textSelection(.enabled)
                    }
                    if let content = detail?.content, !content.isEmpty {
                        Divider()
                        Text(content)
                            .font(.system(.footnote, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            .padding()
        }
        .navigationTitle(skill.name)
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
    }

    private func load() async {
        isLoading = true
        loadError = nil
        defer { isLoading = false }
        do {
            detail = try await skillsStore.loadDetail(name: skill.name)
        } catch {
            loadError = error.localizedDescription
        }
    }
}
