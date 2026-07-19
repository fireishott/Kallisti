import SwiftUI

struct ProfileSelectorSheet: View {
    let profiles: [ProfileStore.HeraldProfile]
    let activeProfileName: String?
    let onSelect: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                if profiles.isEmpty {
                    ContentUnavailableView(
                        "No Profiles",
                        systemImage: "brain.head.profile",
                        description: Text("No Hermes profiles are available.")
                    )
                } else {
                    ForEach(profiles) { profile in
                        Button {
                            onSelect(profile.name)
                            dismiss()
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(profile.name)
                                        .font(.headline)
                                    if !profile.description.isEmpty {
                                        Text(profile.description)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(2)
                                    }
                                    Text("\(profile.skillCount) skills")
                                        .font(.caption2)
                                        .foregroundStyle(.tertiary)
                                }
                                Spacer()
                                if profile.name == activeProfileName {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundStyle(.green)
                                }
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .navigationTitle("Profile")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}
