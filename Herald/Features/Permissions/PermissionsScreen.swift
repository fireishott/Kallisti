import SwiftUI

struct PermissionsScreen: View {
    @Environment(PermissionsStore.self) private var permissionsStore

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            ScrollView {
                VStack(spacing: Design.Spacing.md) {
                    headerText

                    ForEach(permissionsStore.capabilities) { capability in
                        PermissionCard(capability: capability) {
                            if capability.status == .denied {
                                // iOS won't re-prompt after denial — open Settings instead
                                if let url = URL(string: UIApplication.openSettingsURLString) {
                                    UIApplication.shared.open(url)
                                }
                            } else {
                                Task { await permissionsStore.requestPermission(for: capability.permissionType) }
                            }
                        }
                    }
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.sm)
            }
        }
        .navigationTitle("Permissions")
        .task { await permissionsStore.reloadCapabilities() }
    }

    private var headerText: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text("Access")
                .brandEyebrow()
            Text("herald and hermes work together best with your permission. you control what data herald can access.")
                .font(Design.Typography.editorialItalicSmall)
                .foregroundStyle(Design.Colors.foreground)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Design.Spacing.xxs)
    }

}
