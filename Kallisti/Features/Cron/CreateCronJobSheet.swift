import SwiftUI

struct CreateCronJobSheet: View {
    let onCreate: (String, String, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var schedule = ""
    @State private var prompt = ""

    private let presets = [
        ("Every hour", "0 * * * *"),
        ("Daily at 9am", "0 9 * * *"),
        ("Weekdays at 9am", "0 9 * * 1-5"),
        ("Weekly (Monday 9am)", "0 9 * * 1"),
    ]

    var body: some View {
        NavigationStack {
            Form {
                Section("Details") {
                    TextField("Name", text: $name)
                    TextField("Cron expression", text: $schedule)
                        .font(.caption)
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
                        .frame(minHeight: 100)
                }
            }
            .navigationTitle("New Cron Job")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        onCreate(name, schedule, prompt)
                        dismiss()
                    }
                    .disabled(name.isEmpty || schedule.isEmpty || prompt.isEmpty)
                }
            }
        }
    }
}
