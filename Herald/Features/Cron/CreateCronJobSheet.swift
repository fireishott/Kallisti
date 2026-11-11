import SwiftUI

struct CreateCronJobSheet: View {
    let onCreate: (String, String, String, String?) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var schedule = ""
    @State private var prompt = ""
    /// Where the answer goes. `push` is the daemon's own default, so it is the one
    /// this starts on rather than a fourth thing to decide.
    @State private var deliver = "push"

    private let deliveries = ["push", "email", "both", "none"]

    private let presets = [
        ("Every hour", "0 * * * *"),
        ("Daily at 9am", "0 9 * * *"),
        ("Weekdays at 9am", "0 9 * * 1-5"),
        ("Weekly (Monday 9am)", "0 9 * * 1"),
    ]

    private var deliveryNote: String {
        switch deliver {
        case "push": "The paired devices get an alert with the first line of the answer."
        case "email": "One permitted address gets the whole answer. No CC, no BCC."
        case "both": "Push and email."
        default: "The answer is written to its conversation and nothing else happens."
        }
    }

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
                Section("Delivery") {
                    Picker("Where the answer goes", selection: $deliver) {
                        ForEach(deliveries, id: \.self) { channel in
                            Text(channel).tag(channel)
                        }
                    }
                    .pickerStyle(.segmented)
                    Text(deliveryNote)
                        .font(.caption)
                        .foregroundStyle(.secondary)
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
                        onCreate(name, schedule, prompt, deliver)
                        dismiss()
                    }
                    .disabled(name.isEmpty || schedule.isEmpty || prompt.isEmpty)
                }
            }
        }
    }
}
