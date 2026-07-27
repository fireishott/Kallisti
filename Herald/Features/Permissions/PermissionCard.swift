import SwiftUI

struct PermissionCard: View {
    let capability: DeviceCapability
    let onRequest: () -> Void
    @State private var isRequesting = false

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            headerRow
            explanationText
            statusAndAction
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
    }

    // MARK: - Header

    private var headerRow: some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: capability.permissionType.displayIcon)
                .font(.system(size: Design.Size.iconMedium))
                .foregroundStyle(Design.Colors.foreground)
                .frame(width: Design.Size.avatarSmall, height: Design.Size.avatarSmall)
                .background(Design.Colors.surface2)
                .overlay(
                    RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                        .stroke(Design.Colors.border, lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))

            Text(capability.permissionType.displayLabel)
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)

            Spacer()
        }
    }

    // MARK: - Explanation

    private var explanationText: some View {
        Text(capability.permissionType.explanation)
            .font(Design.Typography.callout)
            .foregroundStyle(Design.Colors.secondaryForeground)
            .fixedSize(horizontal: false, vertical: true)
    }

    // MARK: - Status & Action

    private var statusAndAction: some View {
        HStack {
            Label(statusLabelText, systemImage: statusIcon)
                .labelStyle(.titleAndIcon)
                .brandEyebrow(capability.status.displayColor)

            Spacer()

            if let actionLabel = actionLabelText {
                Button {
                    isRequesting = true
                    // Defer the system permission dialog by 100ms to avoid
                    // overlapping with SwiftUI render cycles that can crash
                    // AttributeGraph (BarEnvironmentViewModel visibility update
                    // racing with UIAlertController presentation).
                    Task { @MainActor in
                        try? await Task.sleep(for: .milliseconds(100))
                        onRequest()
                        isRequesting = false
                    }
                } label: {
                    HStack(spacing: 4) {
                        if isRequesting {
                            ProgressView()
                                .scaleEffect(0.7)
                                .tint(Design.Colors.background)
                        }
                        Text(isRequesting ? "Requesting..." : actionLabel)
                            .brandEyebrow(Design.Colors.background)
                    }
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.vertical, Design.Spacing.xs)
                }
                .disabled(isRequesting)
                .background(isRequesting ? Design.Colors.secondaryForeground : Design.Brand.accent)
                .clipShape(Capsule())
            }
        }
    }

    private var statusLabelText: String {
        capability.statusDetail ?? capability.status.displayLabel
    }

    private var actionLabelText: String? {
        if isRequesting { return nil }
        // Health denied/restricted — user must go to Apple Health, not app Settings
        if capability.permissionType == .health,
           capability.status == .denied || capability.status == .restricted {
            return nil
        }
        // Unsupported due to build defect (missing entitlements, device limit) —
        // no Settings action can fix this. Show no button.
        if capability.status == .unsupported {
            // Only show Settings if the detail suggests a user-actionable fix
            if let detail = capability.statusDetail,
               detail.contains("Settings") || detail.contains("Apple Health") {
                return "Open Settings"
            }
            return nil
        }
        return capability.status.actionLabel
    }

    private var statusIcon: String {
        switch capability.status {
        case .authorized, .authorizedWhenInUse, .authorizedAlways: "checkmark.circle.fill"
        case .limited: "exclamationmark.circle.fill"
        case .denied: "xmark.circle.fill"
        case .notDetermined: "questionmark.circle"
        case .restricted: "lock.fill"
        case .unsupported: "nosign"
        }
    }
}
