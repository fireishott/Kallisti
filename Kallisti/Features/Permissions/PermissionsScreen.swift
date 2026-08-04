import SwiftUI

struct PermissionsScreen: View {
    @Environment(PermissionsStore.self) private var permissionsStore

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            ScrollView(.vertical) {
                VStack(spacing: Design.Spacing.md) {
                    headerText

                    ForEach(permissionsStore.capabilities) { capability in
                        PermissionCard(capability: capability) {
                            if capability.status == .denied || capability.status == .restricted {
                                // Denied/restricted — user must enable in system Settings
                                if let url = URL(string: UIApplication.openSettingsURLString) {
                                    UIApplication.shared.open(url)
                                }
                            } else if capability.status == .unsupported {
                                // Unsupported — only open Settings if the detail
                                // text indicates a user-actionable fix (e.g.,
                                // "Manage in Apple Health"). Build defects and
                                // device limits get no action.
                                if let detail = capability.statusDetail,
                                   detail.contains("Settings") || detail.contains("Apple Health") {
                                    if let url = URL(string: UIApplication.openSettingsURLString) {
                                        UIApplication.shared.open(url)
                                    }
                                }
                            } else {
                                // Not determined — request permission
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
