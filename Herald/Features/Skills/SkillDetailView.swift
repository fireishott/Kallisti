import SwiftUI

/// Build 135.41: skill detail view. Loads the real SKILL.md content from the
/// connector's /v1/skills/{name} endpoint and renders it as scrollable text.
/// Falls back to the list metadata (description + path) when the detail fetch
/// fails, so the screen is never a bare shell.
///
/// Editing rides on the Eris control plane only. The editor is drawn when both the
/// transport can carry a write and the daemon marked this skill editable; every
/// other row keeps the screen that shipped.
struct SkillDetailView: View {
    let skill: SkillsStore.HeraldSkill
    @Environment(SkillsStore.self) private var skillsStore

    @State private var detail: SkillsStore.SkillDetail?
    @State private var isLoading = true
    @State private var loadError: String?
    @State private var draft = ""
    @State private var isEditing = false
    @State private var isSaving = false
    @State private var saveError: String?

    /// Both halves are required: a transport with a write behind it, and a daemon
    /// that said this body may be rewritten.
    private var canEdit: Bool { skillsStore.canEditSkills && skill.editable }

    private var hasChanges: Bool { draft != (detail?.content ?? "") }

    var body: some View {
        Group {
            if isEditing {
                editor
            } else {
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
                            reader
                        }
                    }
                    .padding()
                }
            }
        }
        .navigationTitle(skill.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if canEdit && !isEditing {
                ToolbarItem(placement: .primaryAction) {
                    Button("Edit") { beginEditing() }
                }
            }
        }
        .toolbar {
            if isEditing {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { cancelEditing() }
                        .disabled(isSaving)
                }
                ToolbarItem(placement: .confirmationAction) {
                    if isSaving {
                        ProgressView()
                    } else {
                        Button("Save") {
                            Task { await save() }
                        }
                        .disabled(!hasChanges)
                    }
                }
            }
        }
        .task { await load() }
    }

    @ViewBuilder
    private var reader: some View {
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

    /// The body being rewritten. `SKILL.md` is prose and front matter, so the
    /// editor is the same monospaced face the reader draws it in.
    private var editor: some View {
        VStack(alignment: .leading, spacing: 12) {
            if isSaving {
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                    Text("Saving...")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            // The daemon's own sentence on refusal, not a paraphrased failure.
            if let saveError {
                Label(saveError, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            TextEditor(text: $draft)
                .font(.system(.footnote, design: .monospaced))
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .disabled(isSaving)
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(Color.secondary.opacity(0.25), lineWidth: 1)
                )
        }
        .padding()
    }

    private func load() async {
        isLoading = true
        loadError = nil
        defer { isLoading = false }
        do {
            detail = try await skillsStore.loadDetail(key: skill.key)
        } catch {
            loadError = error.localizedDescription
        }
    }

    private func beginEditing() {
        draft = detail?.content ?? ""
        saveError = nil
        isEditing = true
    }

    private func cancelEditing() {
        draft = ""
        saveError = nil
        isEditing = false
    }

    /// Write the body back, then re-read it.
    ///
    /// A re-read rather than an assumption: the daemon is the one that knows what
    /// it stored, so a save that normalised something shows what is actually on
    /// disk. The in-flight guard and the disabled Save are the same rule from two
    /// sides, so a second tap cannot fire a second write.
    private func save() async {
        guard !isSaving else { return }
        isSaving = true
        saveError = nil
        defer { isSaving = false }

        let content = draft
        do {
            try await skillsStore.saveSkill(key: skill.key, content: content)
            await load()
            isEditing = false
        } catch {
            saveError = SkillsStore.refusal(error)
        }
    }
}
