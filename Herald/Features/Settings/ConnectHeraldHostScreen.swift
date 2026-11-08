import SwiftUI

struct ConnectKallistiHostScreen: View {
    @Environment(KallistiHostStore.self) private var hostStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            ScrollView {
                VStack(spacing: Design.Spacing.lg) {
                    hostStatusCard
                    if !isPaired {
                        setupCard
                    }
                    actionsCard

                    if let errorMessage = hostStore.lastErrorMessage ?? pairingStore.lastErrorMessage {
                        errorBanner(message: errorMessage)
                    }
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.lg)
            }
        }
        .navigationTitle("Connect Host")
        .task {
            await hostStore.refresh()
        }
    }

    // MARK: - Host Status

    private var hostStatusCard: some View {
        VStack(spacing: Design.Spacing.lg) {
            Text("Host · Status")
                .brandEyebrow()
                .frame(maxWidth: .infinity, alignment: .leading)

            // Large status icon
            ZStack {
                Circle()
                    .fill(statusColor.opacity(0.12))
                    .overlay(
                        Circle().stroke(statusColor.opacity(0.4), lineWidth: 1)
                    )
                    .frame(width: 80, height: 80)
                Image(systemName: statusIcon)
                    .font(.system(size: 32, weight: .medium))
                    .foregroundStyle(statusColor)
            }

            // Status text
            VStack(spacing: Design.Spacing.xxs) {
                Text(statusTitle)
                    .font(Design.Typography.sectionTitle)
                    .foregroundStyle(Design.Colors.foreground)

                Text(statusSubtitle)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .multilineTextAlignment(.center)
            }

            // Host details (when connected)
            if let host = hostStore.currentHost {
                VStack(spacing: 0) {
                    Divider().overlay(Design.Colors.divider)
                    detailRow(icon: "desktopcomputer", label: host.resolvedDisplayName)
                    Divider().overlay(Design.Colors.divider)
                    detailRow(
                        icon: "clock",
                        label: host.lastSeenAt?.formatted(date: .abbreviated, time: .shortened) ?? "Just now"
                    )
                }
                .padding(.top, Design.Spacing.xs)
            } else if isPaired, let address = pairedAddress {
                // A paired daemon fills no relay host row, so the address is the
                // only detail there is. Showing nothing read as "No Host" with a
                // Disconnect button under it, which is the shape of a broken page.
                VStack(spacing: 0) {
                    Divider().overlay(Design.Colors.divider)
                    detailRow(icon: "network", label: address)
                }
                .padding(.top, Design.Spacing.xs)
            }
        }
        .padding(Design.Spacing.lg)
        .frame(maxWidth: .infinity)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.xl)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xl))
    }

    // MARK: - Setup Instructions

    private var setupCard: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.md) {
            Text("Setup · Terminal")
                .brandEyebrow()

            setupStep(number: "1", command: "kallisti setup", detail: "One-time registration")
            setupStep(number: "2", command: "kallisti pair-phone", detail: "Scan the code in-app")
            setupStep(number: "3", command: "kallisti service install", detail: "Background uptime")
        }
        .padding(Design.Spacing.lg)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.xl)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xl))
    }

    // MARK: - Actions

    private var actionsCard: some View {
        VStack(spacing: 0) {
            if hostStore.currentHost != nil {
                Button(role: .destructive) {
                    Task { await hostStore.revokeCurrentHost() }
                } label: {
                    actionRow(
                        icon: "desktopcomputer.trianglebadge.exclamationmark",
                        label: "Revoke Host",
                        color: Design.Colors.danger
                    )
                }
                .disabled(hostStore.isWorking)

                Divider().overlay(Design.Colors.divider)
            }

            Button {
                Task {
                    await pairingStore.disconnect()
                    dismiss()
                }
            } label: {
                if pairingStore.isWorking {
                    actionRow(
                        icon: "hourglass",
                        label: "Disconnecting",
                        color: Design.Colors.secondaryForeground
                    )
                } else {
                    actionRow(
                        icon: "rectangle.portrait.and.arrow.right",
                        label: "Disconnect",
                        color: Design.Colors.danger
                    )
                }
            }
            .disabled(pairingStore.isWorking)
        }
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.xl)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xl))
    }

    // MARK: - Components

    private func detailRow(icon: String, label: String) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: icon)
                .font(.system(size: 14))
                .foregroundStyle(Design.Colors.secondaryForeground)
                .frame(width: 20)
            Text(label)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
            Spacer()
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    private func setupStep(number: String, command: String, detail: String) -> some View {
        HStack(alignment: .top, spacing: Design.Spacing.sm) {
            Text("00\(number)")
                .brandEyebrow(Design.Brand.accent)
                .frame(width: 28, alignment: .leading)

            VStack(alignment: .leading, spacing: 2) {
                Text("$ \(command)")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
                Text(detail)
                    .brandEyebrow()
            }
        }
    }

    private func actionRow(icon: String, label: String, color: Color) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: icon)
                .font(.system(size: 14))
                .foregroundStyle(color)
                .frame(width: 20)
            Text(label)
                .font(Design.Typography.callout)
                .foregroundStyle(color)
            Spacer()
            Image(systemName: "chevron.right")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(color.opacity(0.5))
        }
        .frame(minHeight: Design.Size.minTapTarget)
        .padding(.horizontal, Design.Spacing.lg)
    }

    private func errorBanner(message: String) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(Design.Colors.warning)
            Text(message)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.foreground)
                .lineLimit(2)
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(Design.Colors.warning.opacity(0.35), lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
    }

    // MARK: - Status Helpers

    /// Whether this device is paired to a host daemon, whatever the relay host row says.
    ///
    /// This screen exists to connect and disconnect the host, so the pair IS the
    /// state it reports. It used to read only the relay-shaped host row, which an
    /// Eris daemon never fills, so a phone that was talking to its daemon read "No
    /// Host" with a Disconnect button underneath it. `isPaired` is the pairing the
    /// app actually uses; the online check is a second chance for a host row that
    /// does arrive.
    private var isPaired: Bool {
        pairingStore.isPaired || hostStore.connectionState == .online
    }

    /// The address this device is paired to, when the pair remembers one.
    private var pairedAddress: String? {
        pairingStore.pairedRelayConfiguration?.baseURLString
    }

    private var statusColor: Color {
        if isPaired { return Design.Colors.success }
        switch hostStore.connectionState {
        case .online:
            return Design.Colors.success
        case .offline, .unreachable:
            return Design.Colors.warning
        case .notConnected:
            return Design.Colors.secondaryForeground
        }
    }

    private var statusIcon: String {
        if isPaired { return "checkmark.circle.fill" }
        switch hostStore.connectionState {
        case .online:
            return "checkmark.circle.fill"
        case .offline:
            return "exclamationmark.circle.fill"
        case .unreachable:
            return "wifi.exclamationmark"
        case .notConnected:
            return "desktopcomputer"
        }
    }

    private var statusTitle: String {
        if isPaired { return "Connected" }
        switch hostStore.connectionState {
        case .online:
            return "Connected"
        case .offline:
            return "Offline"
        case .unreachable:
            return "Status Unavailable"
        case .notConnected:
            return "No Host"
        }
    }

    private var statusSubtitle: String {
        if isPaired {
            return (pairedAddress.map { "Paired to \($0)" }) ?? "Paired to your Agent host"
        }
        switch hostStore.connectionState {
        case .online:
            return "Your Agent is ready"
        case .offline:
            return "Waiting for the Agent connector to come online"
        case .unreachable:
            return hostStore.lastErrorMessage ?? "We couldn't refresh host status from the relay."
        case .notConnected:
            return "Set up the connector on your Agent host"
        }
    }
}
