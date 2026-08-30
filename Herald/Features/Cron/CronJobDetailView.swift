import SwiftUI

/// Build 135.40: rich editor for a cron job. Editable name/schedule/prompt
/// with Save; last/next run and last result remain read-only metadata.
struct CronJobDetailView: View {
    let job: CronStore.CronJob
    @Environment(CronStore.self) private var cronStore
    @Environment(\.dismiss) private var dismiss

    @State private var name: String
    @State private var schedule: String
    @State private var prompt: String
    @State private var isSaving = false
    @State private var saveError: String?
    @State private var saveSuccess = false

    private let presets = [
        ("Every hour", "0 * * * *"),
        ("Daily at 9am", "0 9 * * *"),
        ("Weekdays at 9am", "0 9 * * 1-5"),
        ("Weekly (Monday 9am)", "0 9 * * 1"),
    ]

    init(job: CronStore.CronJob) {
        self.job = job
        _name = State(initialValue: job.name)
        _schedule = State(initialValue: job.schedule)
        _prompt = State(initialValue: job.prompt)
    }

    private var hasChanges: Bool {
        name != job.name || schedule != job.schedule || prompt != job.prompt
    }

    private var isValid: Bool {
        !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !schedule.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        Form {
            Section("Details") {
                TextField("Name", text: $name)
                TextField("Cron expression", text: $schedule)
                    .font(.caption)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
            }

            Section("Quick Presets") {
                ForEach(presets, id: \.1) { preset in
                    Button {
                        schedule = preset.1
                    } label: {
                        HStack {
                            Text(preset.0)
                            Spacer()
                            Text(preset.1)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }

            Section("Prompt") {
                TextEditor(text: $prompt)
                    .frame(minHeight: 140)
                    .font(.callout)
            }

            Section("Schedule") {
                if let lastRun = job.lastRun {
                    LabeledContent("Last Run", value: lastRun.formatted())
                }
                if let nextRun = job.nextRun {
                    LabeledContent("Next Run", value: nextRun.formatted())
                }
            }

            if let result = job.lastResult, !result.isEmpty {
                Section("Last Result") {
                    Text(result)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            if let saveError {
                Section {
                    Label(saveError, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red)
                }
            }

            if saveSuccess {
                Section {
                    Label("Saved", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                }
            }
        }
        .navigationTitle(job.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if isSaving {
                    ProgressView()
                } else {
                    Button("Save") {
                        Task { await save() }
                    }
                    .disabled(!hasChanges || !isValid)
                }
            }
        }
    }

    private func save() async {
        isSaving = true
        saveError = nil
        saveSuccess = false
        defer { isSaving = false }

        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedSchedule = schedule.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedPrompt = prompt.trimmingCharacters(in: .whitespacesAndNewlines)

        do {
            try await cronStore.updateJobContent(id: job.id, name: trimmedName, schedule: trimmedSchedule, prompt: trimmedPrompt)
            saveSuccess = true
        } catch {
            saveError = error.localizedDescription
        }
    }
}
