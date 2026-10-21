import SwiftUI

// MARK: - Eris surface
//
// The one settings entry point for the Eris daemon. It shows the pairing form
// when this device has no usable pair and the paired surface (chat,
// conversations, settings) when it does, so the same route serves first pairing,
// re-pairing after an expiry, and a relaunch that lands straight back in.
//
// Camera denial is not a dead end: the manual fields are always on the form, and
// the scanner sheet explains itself and offers the same fallback.

/// Routed entry: pairing when unpaired, the paired surface when paired.
struct ErisSurfaceScreen: View {
    @State private var store = ErisDaemonStore()
    @State private var isScannerPresented = false

    var body: some View {
        Group {
            if store.chat != nil, let baseURL = store.pairedBaseURL {
                ErisPairedSurface(store: store)
                    // A different daemon is a different surface.
                    .id(baseURL.absoluteString)
            } else {
                ErisPairingForm(store: store, isScannerPresented: $isScannerPresented)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Design.Colors.background.ignoresSafeArea())
        .navigationTitle("Eris")
        .navigationBarTitleDisplayMode(.inline)
        .task { await store.start() }
        .sheet(isPresented: $isScannerPresented) {
            ErisScannerSheet(store: store) {
                isScannerPresented = false
            }
        }
    }
}

// MARK: - Pairing form

/// Manual entry plus the scanner, with the honest state of the last attempt.
struct ErisPairingForm: View {
    @Bindable var store: ErisDaemonStore
    @Binding var isScannerPresented: Bool

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Design.Spacing.lg) {
                header
                scanButton
                stateReport
                manualEntry
                forgetButton
            }
            .padding(Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
        }
    }

    // MARK: Sections

    private var header: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text("Pair this device")
                .font(Design.Typography.screenTitle2)
                .foregroundStyle(Design.Colors.foreground)
            Text("Scan the code the Eris daemon shows, or type its address and code.")
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
        .padding(.top, Design.Spacing.sm)
    }

    private var scanButton: some View {
        Button {
            isScannerPresented = true
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: "qrcode.viewfinder")
                    .font(.system(size: Design.Size.iconSmall, weight: .semibold))
                Text("Scan pairing code")
                    .font(Design.Typography.headline)
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            .foregroundStyle(Design.Colors.foreground)
            .padding(Design.Spacing.md)
            .frame(minHeight: Design.Size.minTapTarget)
            .background(Design.Colors.surface)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        }
        .buttonStyle(.plain)
        .disabled(store.isBusy)
        .accessibilityIdentifier("eris.scanPairingCode")
    }

    /// Whatever the last attempt actually produced, in the order a user needs it.
    @ViewBuilder
    private var stateReport: some View {
        if case .connecting = store.phase {
            HStack(spacing: Design.Spacing.sm) {
                ProgressView().tint(Design.Brand.accent)
                Text("Connecting to \(store.addressText)…")
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
        if store.isVerifying {
            HStack(spacing: Design.Spacing.sm) {
                ProgressView().tint(Design.Brand.accent)
                Text("Checking the stored pairing…")
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
        if let failure = store.failure {
            VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
                Text(failure)
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.danger)
                    .fixedSize(horizontal: false, vertical: true)
                if let code = store.failureCode, !code.isEmpty {
                    Text("code: \(code)")
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Design.Spacing.sm)
            .background(Design.Colors.surface)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
            .accessibilityIdentifier("eris.pairingFailure")
        }
        if let notice = store.notice {
            Text(notice)
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.warning)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var manualEntry: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Text("Daemon address")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)
            TextField("192.168.1.20:7341", text: $store.addressText)
                .font(Design.Typography.body)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
                .padding(Design.Spacing.sm)
                .background(Design.Colors.surface)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
                .accessibilityIdentifier("eris.daemonAddress")

            Text("Pairing code")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)
            TextField("AB2CD3EF", text: $store.codeText)
                .font(Design.Typography.code)
                .textInputAutocapitalization(.characters)
                .autocorrectionDisabled()
                .padding(Design.Spacing.sm)
                .background(Design.Colors.surface)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
                .accessibilityIdentifier("eris.pairingCode")

            Text("Device name")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)
            TextField("Kallisti", text: $store.deviceName)
                .font(Design.Typography.body)
                .autocorrectionDisabled()
                .padding(Design.Spacing.sm)
                .background(Design.Colors.surface)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
                .accessibilityIdentifier("eris.deviceName")

            Button {
                Task { await store.pairFromFields() }
            } label: {
                Text("Pair with this code")
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Colors.background)
                    .frame(maxWidth: .infinity, minHeight: Design.Size.minTapTarget)
                    .background(Design.Brand.accent)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
            }
            .buttonStyle(.plain)
            .disabled(store.isBusy || trimmedAddress.isEmpty || trimmedCode.isEmpty)
            .opacity(store.isBusy || trimmedAddress.isEmpty || trimmedCode.isEmpty ? 0.55 : 1)
            .padding(.top, Design.Spacing.xxs)
            .accessibilityIdentifier("eris.pairWithCode")

            Text("A bare `host:port` is treated as http. Eris serves its device API on 7341.")
                .font(Design.Typography.helper)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var forgetButton: some View {
        if store.hasStoredDaemon {
            Button(role: .destructive) {
                Task { await store.forget() }
            } label: {
                Text("Forget this daemon")
                    .font(Design.Typography.callout)
                    .frame(maxWidth: .infinity, minHeight: Design.Size.minTapTarget)
            }
            .buttonStyle(.plain)
            .foregroundStyle(Design.Colors.danger)
            .accessibilityIdentifier("eris.forgetDaemon")
        }
    }

    private var trimmedAddress: String {
        store.addressText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var trimmedCode: String {
        store.codeText.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

// MARK: - Scanner sheet

/// The camera sheet. A denial or a missing camera replaces the preview with a
/// readable reason; the manual form behind the sheet is untouched either way.
struct ErisScannerSheet: View {
    let store: ErisDaemonStore
    let onDismiss: () -> Void

    @State private var unavailableReason: String?

    var body: some View {
        NavigationStack {
            ZStack {
                Color.black.ignoresSafeArea()
                if let reason = unavailableReason {
                    unavailable(reason)
                } else {
                    ErisQRScannerView(
                        onPayload: { payload in
                            Task { await store.pair(payload: payload) }
                        },
                        onUnavailable: { reason in
                            unavailableReason = reason
                        }
                    )
                    .ignoresSafeArea()
                    scannerOverlay
                }
            }
            .navigationTitle("Scan code")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: onDismiss)
                }
            }
        }
        .onChange(of: store.phase) { _, phase in
            if case .paired = phase { onDismiss() }
        }
        .onChange(of: store.failure) { _, failure in
            // A payload that cannot be redeemed closes the sheet, because the
            // typed reason belongs on the form that also offers manual entry.
            if failure != nil { onDismiss() }
        }
    }

    private var scannerOverlay: some View {
        VStack {
            Spacer()
            VStack(spacing: Design.Spacing.xs) {
                Text(store.isBusy ? "Pairing…" : "Point the camera at the daemon's QR code")
                    .font(Design.Typography.footnote)
                    .foregroundStyle(.white)
                if store.isBusy {
                    ProgressView().tint(.white)
                } else {
                    Button("Enter manually") { onDismiss() }
                        .font(Design.Typography.footnote)
                        .foregroundStyle(.white)
                }
            }
            .padding(Design.Spacing.md)
            .frame(maxWidth: .infinity)
            .background(.black.opacity(0.55))
            .padding(.bottom, Design.Spacing.xl)
        }
    }

    private func unavailable(_ reason: String) -> some View {
        VStack(spacing: Design.Spacing.md) {
            Image(systemName: "camera.fill")
                .font(.system(size: Design.Size.iconLarge))
                .foregroundStyle(.white.opacity(0.7))
            Text(reason)
                .font(Design.Typography.footnote)
                .foregroundStyle(.white)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Button("Enter manually", action: onDismiss)
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.background)
                .padding(.horizontal, Design.Spacing.lg)
                .padding(.vertical, Design.Spacing.sm)
                .background(Design.Brand.accent)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
            if let failure = store.failure {
                Text(failure)
                    .font(Design.Typography.helper)
                    .foregroundStyle(Design.Colors.danger)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(Design.Spacing.lg)
    }
}
