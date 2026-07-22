import SwiftUI
import UIKit

// MARK: - Step Model

enum OnboardingStep: Int, CaseIterable {
    case welcome
    case relay
    case pairing
    case permissions
    case ready

    /// Position in the progress bar. Welcome has no progress (it's the intro).
    var progressIndex: Int? {
        switch self {
        case .welcome: nil
        case .relay: 1
        case .pairing: 2
        case .permissions: 3
        case .ready: 4
        }
    }

    var stepKicker: String? {
        switch self {
        case .welcome: nil
        case .relay: "001 · RELAY"
        case .pairing: "002 · HANDSHAKE"
        case .permissions: "003 · PERMISSIONS"
        case .ready: "004 · PAIRED"
        }
    }
}

// MARK: - Root Flow

struct OnboardingFlowView: View {
    @Environment(PairingStore.self) private var pairingStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(PermissionsStore.self) private var permissionsStore

    @State private var step: OnboardingStep
    @State private var setupCode: String = ""
    @State private var isScannerPresented: Bool = false
    @State private var localErrorMessage: String?
    @FocusState private var isSetupCodeFocused: Bool
    @FocusState private var isRelayURLFocused: Bool

    init(initialStep: OnboardingStep) {
        _step = State(initialValue: initialStep)
    }

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            VStack(spacing: 0) {
                OnboardingProgressBar(step: step.progressIndex ?? 0, total: 4)
                    .opacity(step.progressIndex == nil ? 0 : 1)
                    .padding(.top, Design.Spacing.xs)

                if let kicker = step.stepKicker, let index = step.progressIndex {
                    OnboardingStepHeader(kicker: kicker, step: index, total: 4)
                        .padding(.top, Design.Spacing.sm)
                }

                content
            }
        }
        .sheet(isPresented: $isScannerPresented) {
            scannerSheet
        }
        .onChange(of: setupCode) { _, newValue in
            let formatted = PhonePairingCode.format(newValue)
            if formatted != newValue {
                setupCode = formatted
            }
        }
        .animation(Design.Motion.standard, value: step)
    }

    @ViewBuilder
    private var content: some View {
        switch step {
        case .welcome:
            WelcomeStepView(onAdvance: { advance(to: .relay) })
        case .relay:
            RelayStepView(
                relayConfiguration: relayConfiguration,
                validationMessage: relayConfiguration.validationMessage,
                errorMessage: localErrorMessage,
                connectionModeBinding: connectionModeBinding,
                customRelayURLBinding: customRelayURLBinding,
                isRelayURLFocused: $isRelayURLFocused,
                onPaste: pasteRelayURL,
                onScan: { isScannerPresented = true },
                onContinue: { advance(to: .pairing) }
            )
        case .pairing:
            PairingStepView(
                setupCode: $setupCode,
                isSetupCodeFocused: $isSetupCodeFocused,
                isWorking: pairingStore.isWorking,
                isValid: PhonePairingCode.isComplete(setupCode) && relayConfiguration.validationMessage == nil,
                errorMessage: errorMessage,
                onScan: { isScannerPresented = true },
                onPair: { Task { await completePairing(using: setupCode) } }
            )
        case .permissions:
            PermissionsStepView(
                capabilities: onboardingCapabilities,
                onRequest: { type in Task { await permissionsStore.requestPermission(for: type) } },
                onContinue: { advance(to: .ready) }
            )
            .task { await permissionsStore.reloadCapabilities() }
        case .ready:
            ReadyStepView(
                hostDisplayName: pairingStore.pairedRelayConfiguration?.hostDisplayName
                    ?? relayConfiguration.relayOriginLabel,
                onOpen: { pairingStore.completePermissionsOnboarding() }
            )
        }
    }

    // MARK: - Scanner

    private var scannerSheet: some View {
        Group {
            if SetupCodeScannerView.isScannerAvailable {
                SetupCodeScannerView(
                    onCodeDetected: { scannedValue in
                        isScannerPresented = false
                        handleScannedValue(scannedValue)
                    },
                    onFailure: { message in
                        isScannerPresented = false
                        localErrorMessage = message
                    }
                )
                .ignoresSafeArea()
            } else {
                ZStack {
                    Design.Colors.background.ignoresSafeArea()
                    VStack(spacing: Design.Spacing.md) {
                        Text("SCANNER UNAVAILABLE")
                            .brandEyebrow(Design.Colors.foreground)
                        Text("enter the code manually.")
                            .font(Design.Typography.editorialItalicSmall)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Button("Dismiss") { isScannerPresented = false }
                            .brandEyebrow(Design.Colors.foreground)
                            .padding(.horizontal, Design.Spacing.lg)
                            .padding(.vertical, Design.Spacing.sm)
                            .overlay(Capsule().stroke(Design.Colors.borderStrong, lineWidth: 1))
                    }
                }
                .presentationDetents([.medium])
            }
        }
    }

    // MARK: - Transitions & Side Effects

    private func advance(to next: OnboardingStep) {
        localErrorMessage = nil
        step = next
    }

    private func pasteRelayURL() {
        guard let pasted = UIPasteboard.general.string else { return }
        var config = settingsStore.settings.relayConfiguration
        config.updateConnectionMode(.selfHostedRelay)
        config.customRelayBaseURL = pasted.trimmingCharacters(in: .whitespacesAndNewlines)
        settingsStore.settings.relayConfiguration = config
    }

    /// Parse a QR code value — either JSON `{"code":"...","relay":"..."}` or plain code.
    /// When JSON includes relay, auto-configures the relay before pairing.
    private func handleScannedValue(_ value: String) {
        if let data = value.data(using: .utf8),
           let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let code = json["code"] as? String {
            if let relay = json["relay"] as? String, !relay.isEmpty {
                applyRelayURL(relay)
            }
            setupCode = PhonePairingCode.format(code)
            Task { await completePairing(using: code) }
            return
        }

        // Fall back to plain pairing code (backward compatible)
        if step == .relay {
            step = .pairing
        }
        setupCode = PhonePairingCode.format(value)
        Task { await completePairing(using: value) }
    }

    private func applyRelayURL(_ rawRelayURL: String) {
        var config = settingsStore.settings.relayConfiguration
        config.updateConnectionMode(.selfHostedRelay)
        config.customRelayBaseURL = rawRelayURL
        settingsStore.settings.relayConfiguration = config
    }

    private func completePairing(using rawCode: String) async {
        guard relayConfiguration.validationMessage == nil else {
            localErrorMessage = relayConfiguration.validationMessage
            return
        }
        let didPair = await pairingStore.pair(using: rawCode)
        if didPair {
            localErrorMessage = nil
            step = .permissions
        } else if pairingStore.lastErrorMessage == nil {
            localErrorMessage = PhonePairingCodeError.invalidFormat.localizedDescription
        }
    }

    // MARK: - Bindings

    private var relayConfiguration: RelayConfiguration {
        settingsStore.settings.relayConfiguration
    }

    private var errorMessage: String? {
        pairingStore.lastErrorMessage ?? localErrorMessage
    }

    private var onboardingCapabilities: [DeviceCapability] {
        permissionsStore.capabilities.filter { capability in
            PermissionType.onboardingPermissions.contains(capability.permissionType)
        }
    }

    private var connectionModeBinding: Binding<RelayConnectionMode> {
        Binding(
            get: { settingsStore.settings.relayConfiguration.connectionMode },
            set: { newValue in
                var config = settingsStore.settings.relayConfiguration
                config.updateConnectionMode(newValue)
                settingsStore.settings.relayConfiguration = config
            }
        )
    }

    private var customRelayURLBinding: Binding<String> {
        Binding(
            get: { settingsStore.settings.relayConfiguration.customRelayBaseURL },
            set: { newValue in
                var config = settingsStore.settings.relayConfiguration
                config.customRelayBaseURL = newValue.trimmingCharacters(in: .whitespacesAndNewlines)
                settingsStore.settings.relayConfiguration = config
            }
        )
    }
}

// MARK: - Step 00 · Welcome

private struct WelcomeStepView: View {
    let onAdvance: () -> Void

    var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 0) {
                // Brand row
                HStack(alignment: .center, spacing: Design.Spacing.xs) {
                    Text("HERMES · iOS")
                        .brandEyebrow(Design.Colors.foreground)
                    Spacer()
                    let appVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown"
                    Text("v\(appVersion)")
                        .brandEyebrow()
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.top, Design.Spacing.lg)

            Spacer(minLength: Design.Spacing.xxl)

            // Huge mono display
            VStack(alignment: .leading, spacing: Design.Spacing.md) {
                VStack(alignment: .leading, spacing: 0) {
                    Text("Hermes")
                    Text("on")
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                    Text("iOS.")
                }
                .font(.system(size: 64, weight: .regular, design: .monospaced))
                .tracking(-3.2)
                .textCase(.uppercase)
                .foregroundStyle(Design.Colors.foreground)
                .lineSpacing(-6)

                Text("An iOS client for the\nHermes Agent framework.")
                    .font(Design.Typography.editorialItalic)
                    .foregroundStyle(Design.Colors.foreground.opacity(0.9))
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, Design.Spacing.md)

            Spacer()

            // Eyebrow rail + CTA
            VStack(spacing: Design.Spacing.md) {
                Rectangle()
                    .fill(Design.Colors.border)
                    .frame(height: 1)

                HStack {
                    Text("CHAT").brandEyebrow()
                    Spacer()
                    Text("VOICE").brandEyebrow()
                    Spacer()
                    Text("VISION").brandEyebrow()
                    Spacer()
                    Text("SENSORS").brandEyebrow()
                }

                OnboardingPrimaryCta(title: "Begin") { onAdvance() }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
            }
        }
    }
}

// MARK: - Step 01 · Relay

private struct RelayStepView: View {
    let relayConfiguration: RelayConfiguration
    let validationMessage: String?
    let errorMessage: String?
    let connectionModeBinding: Binding<RelayConnectionMode>
    let customRelayURLBinding: Binding<String>
    var isRelayURLFocused: FocusState<Bool>.Binding
    let onPaste: () -> Void
    let onScan: () -> Void
    let onContinue: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Title block
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                Text("Endpoint.")
                    .font(.system(size: 42, weight: .regular, design: .monospaced))
                    .tracking(-1.9)
                    .textCase(.uppercase)
                    .foregroundStyle(Design.Colors.foreground)

                Text("where your Herald is reachable.")
                    .font(Design.Typography.editorialItalicSmall)
                    .foregroundStyle(Design.Colors.foreground.opacity(0.85))
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.top, Design.Spacing.md)

            // URL field + mode
            ScrollView {
                VStack(alignment: .leading, spacing: Design.Spacing.md) {
                    ConnectionModeSelector(
                        modes: relayConfiguration.selectableConnectionModes,
                        selection: connectionModeBinding
                    )

                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        Text("RELAY URL").brandEyebrow()

                        if relayConfiguration.connectionMode.usesCustomRelayURL {
                            TextField(urlPlaceholder(for: relayConfiguration.connectionMode), text: customRelayURLBinding)
                                .textInputAutocapitalization(.never)
                                .keyboardType(.URL)
                                .autocorrectionDisabled()
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)
                                .padding(Design.Spacing.md)
                                .background(Design.Colors.surface, in: RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                                .overlay(
                                    RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                                        .stroke(Design.Colors.foreground.opacity(0.5), lineWidth: 1)
                                )
                                .focused(isRelayURLFocused)
                                .accessibilityLabel("Relay URL")

                            if let hint = relayConfiguration.connectionMode.relayURLHint {
                                Text(hint)
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        } else if let hostedRelayBaseURL = relayConfiguration.hostedRelayBaseURL {
                            Text(hostedRelayBaseURL)
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                                .padding(Design.Spacing.md)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(Design.Colors.surface, in: RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                        }

                        if let validationMessage {
                            Text(validationMessage)
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.warning)
                        }

                        Text(relayConfiguration.connectionMode.backgroundDeliveryNote)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .fixedSize(horizontal: false, vertical: true)
                    }

                    if let errorMessage {
                        Text(errorMessage)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.danger)
                    }
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.top, Design.Spacing.lg)
            }

            Spacer()

            // Action rail
            VStack(spacing: Design.Spacing.sm) {
                HStack(spacing: Design.Spacing.xs) {
                    OnboardingGhostCta(title: "PASTE", action: onPaste)
                    OnboardingGhostCta(title: "SCAN QR", action: onScan)
                }

                OnboardingPrimaryCta(title: "Continue") { onContinue() }
                    .disabled(validationMessage != nil)
                    .opacity(validationMessage == nil ? 1 : 0.5)
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
        }
    }
}

private func urlPlaceholder(for mode: RelayConnectionMode) -> String {
    switch mode {
    case .tailscale:
        return "https://my-mac.tail-scale.ts.net/v1"
    case .selfHostedRelay:
        return "https://relay.example.com/v1"
    }
}

private struct ConnectionModeSelector: View {
    let modes: [RelayConnectionMode]
    let selection: Binding<RelayConnectionMode>

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text("CONNECTION MODE").brandEyebrow()

            VStack(spacing: Design.Spacing.xs) {
                ForEach(modes, id: \.self) { mode in
                    Button {
                        selection.wrappedValue = mode
                    } label: {
                        HStack(alignment: .top, spacing: Design.Spacing.sm) {
                            Image(systemName: iconName(for: mode))
                                .font(.system(size: 15, weight: .semibold))
                                .foregroundStyle(selection.wrappedValue == mode ? Design.Brand.accent : Design.Colors.secondaryForeground)
                                .frame(width: 20)

                            VStack(alignment: .leading, spacing: 3) {
                                Text(mode.displayLabel)
                                    .font(Design.Typography.callout)
                                    .foregroundStyle(Design.Colors.foreground)
                                Text(mode.shortDescription)
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                    .fixedSize(horizontal: false, vertical: true)
                            }

                            Spacer(minLength: Design.Spacing.sm)

                            if selection.wrappedValue == mode {
                                Image(systemName: "checkmark.circle.fill")
                                    .font(.system(size: 16, weight: .semibold))
                                    .foregroundStyle(Design.Brand.accent)
                            }
                        }
                        .padding(Design.Spacing.md)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Design.Colors.surface, in: RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                        .overlay(
                            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                                .stroke(selection.wrappedValue == mode ? Design.Brand.accent.opacity(0.7) : Design.Colors.border, lineWidth: 1)
                        )
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    private func iconName(for mode: RelayConnectionMode) -> String {
        switch mode {
        case .tailscale:
            return "point.3.connected.trianglepath.dotted"
        case .selfHostedRelay:
            return "server.rack"
        }
    }
}

// MARK: - Step 02 · Pairing

private struct PairingStepView: View {
    @Binding var setupCode: String
    var isSetupCodeFocused: FocusState<Bool>.Binding
    let isWorking: Bool
    let isValid: Bool
    let errorMessage: String?
    let onScan: () -> Void
    let onPair: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Title block
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                Text("Pairing\ncode.")
                    .font(.system(size: 40, weight: .regular, design: .monospaced))
                    .tracking(-1.8)
                    .textCase(.uppercase)
                    .foregroundStyle(Design.Colors.foreground)
                    .lineSpacing(-4)

                Text("printed by `herald-connector pair-phone`.")
                    .font(Design.Typography.editorialItalicSmall)
                    .foregroundStyle(Design.Colors.foreground.opacity(0.85))
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.top, Design.Spacing.md)

            // Boxed code input
            ScrollView {
                VStack(alignment: .leading, spacing: Design.Spacing.md) {
                    pairingCodeBlock
                        .padding(.top, Design.Spacing.lg)

                    Text("— 8 CHARS. CASE-INSENSITIVE. DASHES OPTIONAL.")
                        .brandEyebrow()

                    if let errorMessage {
                        Text(errorMessage)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.danger)
                    }
                }
                .padding(.horizontal, Design.Spacing.md)
            }

            Spacer()

            VStack(spacing: Design.Spacing.sm) {
                OnboardingGhostCta(title: "SCAN QR", action: onScan)

                Button(action: onPair) {
                    Group {
                        if isWorking {
                            ProgressView().tint(Design.Colors.background)
                        } else {
                            HStack(spacing: Design.Spacing.xs) {
                                Text("Connect Herald")
                                Text("→").accessibilityHidden(true)
                            }
                            .font(Design.Typography.headline)
                            .tracking(0.5)
                            .textCase(.uppercase)
                            .foregroundStyle(Design.Colors.background)
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, Design.Spacing.sm + 2)
                }
                .background(Design.Brand.accent)
                .clipShape(Capsule())
                .disabled(isWorking || !isValid)
                .opacity(isWorking || !isValid ? 0.5 : 1)
                .accessibilityLabel("Connect Herald")
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
        }
    }

    private var pairingCodeBlock: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            HStack {
                Text("CODE").brandEyebrow()
                Spacer()
            }

            ZStack {
                // Visible boxes
                HStack(spacing: 4) {
                    ForEach(0..<8, id: \.self) { i in
                        codeSlot(at: i)
                    }
                }

                // Invisible TextField drives state + keyboard
                TextField("", text: $setupCode)
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                    .keyboardType(.asciiCapable)
                    .focused(isSetupCodeFocused)
                    .foregroundStyle(.clear)
                    .tint(.clear)
                    .accentColor(.clear)
                    .accessibilityLabel("Setup code")
                    .frame(height: 52)
                    .opacity(0.02)
            }
            .contentShape(Rectangle())
            .onTapGesture { isSetupCodeFocused.wrappedValue = true }
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(Design.Colors.borderStrong, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
    }

    private func codeSlot(at index: Int) -> some View {
        let char = charAt(index)
        let filled = char != nil
        return VStack(spacing: 4) {
            Text(char.map(String.init) ?? "·")
                .font(.system(size: 24, weight: .semibold, design: .monospaced))
                .foregroundStyle(filled ? Design.Colors.foreground : Design.Colors.tertiaryForeground)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Design.Spacing.xs)
            Rectangle()
                .fill(filled ? Design.Brand.accent : Design.Colors.border)
                .frame(height: 2)
        }
    }

    private func charAt(_ index: Int) -> Character? {
        // Walk the formatted string and extract the non-dash characters.
        let stripped = setupCode.replacingOccurrences(of: "-", with: "")
        guard index < stripped.count else { return nil }
        return stripped[stripped.index(stripped.startIndex, offsetBy: index)]
    }
}

// MARK: - Step 03 · Permissions

private struct PermissionsStepView: View {
    let capabilities: [DeviceCapability]
    let onRequest: (PermissionType) -> Void
    let onContinue: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                Text("System\naccess.")
                    .font(.system(size: 38, weight: .regular, design: .monospaced))
                    .tracking(-1.6)
                    .textCase(.uppercase)
                    .foregroundStyle(Design.Colors.foreground)
                    .lineSpacing(-4)

                Text("granted per-capability by iOS.")
                    .font(Design.Typography.editorialItalicSmall)
                    .foregroundStyle(Design.Colors.foreground.opacity(0.85))
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.top, Design.Spacing.md)

            ScrollView {
                VStack(spacing: 0) {
                    Rectangle().fill(Design.Colors.border).frame(height: 1)
                    ForEach(Array(capabilities.enumerated()), id: \.element.id) { index, capability in
                        permissionRow(index: index, capability: capability)
                        Rectangle().fill(Design.Colors.border).frame(height: 1)
                    }
                }
                .padding(.top, Design.Spacing.lg)
            }

            VStack(spacing: Design.Spacing.sm) {
                Text("REVOCABLE IN iOS SETTINGS AT ANY TIME.")
                    .brandEyebrow()
                    .frame(maxWidth: .infinity)

                OnboardingPrimaryCta(title: "Continue") { onContinue() }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
        }
    }

    private func permissionRow(index: Int, capability: DeviceCapability) -> some View {
        let n = String(format: "%02d", index + 1)
        return HStack(alignment: .top, spacing: Design.Spacing.sm) {
            Text(n)
                .font(Design.Typography.caption2)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .tracking(1.0)
                .frame(width: 22, alignment: .leading)
                .padding(.top, 2)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: Design.Spacing.xs) {
                    Circle()
                        .fill(statusDot(for: capability.status))
                        .frame(width: 6, height: 6)
                    Text(capability.permissionType.terseLabel)
                        .font(Design.Typography.footnote)
                        .tracking(0.4)
                        .textCase(.uppercase)
                        .foregroundStyle(Design.Colors.foreground)
                }

                Text(capability.permissionType.terseExplanation)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .fixedSize(horizontal: false, vertical: true)

                Text("— \(capability.permissionType.requirementTag)")
                    .brandEyebrow()
            }

            Spacer()

            statusPill(for: capability)
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
    }

    @ViewBuilder
    private func statusPill(for capability: DeviceCapability) -> some View {
        switch capability.status {
        case .authorized, .authorizedWhenInUse, .authorizedAlways, .limited:
            Text("GRANTED")
                .brandEyebrow(Design.Colors.foreground.opacity(0.7))
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.vertical, 6)
                .overlay(Capsule().stroke(Design.Colors.borderStrong, lineWidth: 1))

        case .notDetermined:
            Button(action: { onRequest(capability.permissionType) }) {
                Text("ALLOW")
                    .brandEyebrow(Design.Colors.background)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, 6)
            }
            .background(Design.Colors.foreground)
            .clipShape(Capsule())

        case .denied:
            Button(action: openSystemSettings) {
                Text("SETTINGS")
                    .brandEyebrow(Design.Colors.warning)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, 6)
            }
            .overlay(Capsule().stroke(Design.Colors.warning.opacity(0.4), lineWidth: 1))

        case .restricted, .unsupported:
            Text("N/A")
                .brandEyebrow(Design.Colors.tertiaryForeground)
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.vertical, 6)
                .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
        }
    }

    private func statusDot(for status: PermissionStatus) -> Color {
        switch status {
        case .authorized, .authorizedWhenInUse, .authorizedAlways, .limited:
            Design.Colors.success
        case .notDetermined:
            Design.Brand.accent
        case .denied:
            Design.Colors.warning
        case .restricted, .unsupported:
            Design.Colors.tertiaryForeground
        }
    }

    private func openSystemSettings() {
        if let url = URL(string: UIApplication.openSettingsURLString) {
            UIApplication.shared.open(url)
        }
    }
}

// MARK: - Step 04 · Ready

private struct ReadyStepView: View {
    let hostDisplayName: String
    let onOpen: () -> Void

    var body: some View {
        ZStack {
            // Background glyph
            AppIconWatermark()

            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: Design.Spacing.md) {
                    Text("Paired.")
                        .font(.system(size: 72, weight: .regular, design: .monospaced))
                        .tracking(-4.2)
                        .textCase(.uppercase)
                        .foregroundStyle(Design.Colors.foreground)

                    Text("Handshake complete.\nSession key installed\nin the iOS keychain.")
                        .font(Design.Typography.editorialItalic)
                        .foregroundStyle(Design.Colors.foreground.opacity(0.9))
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.top, Design.Spacing.xl)

                VStack(spacing: 0) {
                    HStack {
                        Text("RELAY").brandEyebrow()
                        Spacer()
                        Text(hostDisplayName)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.foreground)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .padding(.vertical, Design.Spacing.xs)
                    Rectangle().fill(Design.Colors.border).frame(height: 1)
                }
                .padding(.top, Design.Spacing.lg)

                Spacer()

                OnboardingPrimaryCta(title: "Open app") { onOpen() }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
        }
    }
}

// MARK: - App Icon Watermark

/// Responsive app icon watermark for backgrounds. Scales to 1.2x screen
/// width (capped at 500pt) so it fits all device sizes.
private struct AppIconWatermark: View {
    var body: some View {
        GeometryReader { geo in
            let markSize = min(geo.size.width * 1.2, 500)
            Image("AppIconImage")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: markSize, height: markSize)
                .opacity(0.06)
                .offset(x: geo.size.width * 0.2, y: -40)
        }
    }
}

// MARK: - Atoms

struct OnboardingProgressBar: View {
    let step: Int
    let total: Int

    var body: some View {
        HStack(spacing: 4) {
            ForEach(0..<total, id: \.self) { i in
                Rectangle()
                    .fill(i < step ? Design.Colors.foreground : Design.Colors.surface2)
                    .frame(height: 2)
            }
        }
        .padding(.horizontal, Design.Spacing.md)
    }
}

struct OnboardingStepHeader: View {
    let kicker: String
    let step: Int
    let total: Int

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(kicker).brandEyebrow()
            Spacer()
            Text("step \(step)/\(total)")
                .brandEyebrow(Design.Colors.tertiaryForeground)
        }
        .padding(.horizontal, Design.Spacing.md)
    }
}

struct OnboardingPrimaryCta: View {
    let title: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: Design.Spacing.xs) {
                Text(title)
                Text("→").accessibilityHidden(true)
            }
            .font(Design.Typography.headline)
            .tracking(0.5)
            .textCase(.uppercase)
            .foregroundStyle(Design.Colors.background)
            .frame(maxWidth: .infinity)
            .padding(.vertical, Design.Spacing.sm + 2)
        }
        .background(Design.Brand.accent)
        .clipShape(Capsule())
        .accessibilityLabel(title)
    }
}

struct OnboardingGhostCta: View {
    let title: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .brandEyebrow(Design.Colors.foreground.opacity(0.85))
                .frame(maxWidth: .infinity)
                .padding(.vertical, Design.Spacing.sm)
        }
        .overlay(Capsule().stroke(Design.Colors.borderStrong, lineWidth: 1))
    }
}

// MARK: - Herald Mark

struct HeraldMark: View {
    let size: CGFloat
    let color: Color

    var body: some View {
        Canvas { context, canvasSize in
            let scale = min(canvasSize.width, canvasSize.height) / 77.0
            var path = Path()

            // Right pillar
            path.move(to: CGPoint(x: 54.414 * scale, y: 0))
            path.addLine(to: CGPoint(x: 77.001 * scale, y: 0))
            path.addLine(to: CGPoint(x: 77.001 * scale, y: 63.653 * scale))
            path.addLine(to: CGPoint(x: 54.414 * scale, y: 63.653 * scale))
            path.addLine(to: CGPoint(x: 54.414 * scale, y: 56.467 * scale))
            path.addLine(to: CGPoint(x: 58.52 * scale, y: 56.467 * scale))
            path.addLine(to: CGPoint(x: 58.52 * scale, y: 13.347 * scale))
            path.addLine(to: CGPoint(x: 54.414 * scale, y: 13.347 * scale))
            path.closeSubpath()

            // Middle pillar
            path.move(to: CGPoint(x: 49.794 * scale, y: 13.347 * scale))
            path.addLine(to: CGPoint(x: 49.794 * scale, y: 6.673 * scale))
            path.addLine(to: CGPoint(x: 27.207 * scale, y: 6.673 * scale))
            path.addLine(to: CGPoint(x: 27.207 * scale, y: 20.533 * scale))
            path.addLine(to: CGPoint(x: 31.313 * scale, y: 20.533 * scale))
            path.addLine(to: CGPoint(x: 31.313 * scale, y: 63.653 * scale))
            path.addLine(to: CGPoint(x: 27.207 * scale, y: 63.653 * scale))
            path.addLine(to: CGPoint(x: 27.207 * scale, y: 70.327 * scale))
            path.addLine(to: CGPoint(x: 49.794 * scale, y: 70.327 * scale))
            path.addLine(to: CGPoint(x: 49.794 * scale, y: 56.467 * scale))
            path.addLine(to: CGPoint(x: 45.687 * scale, y: 56.467 * scale))
            path.addLine(to: CGPoint(x: 45.687 * scale, y: 13.347 * scale))
            path.closeSubpath()

            // Left pillar
            path.move(to: CGPoint(x: 22.587 * scale, y: 13.347 * scale))
            path.addLine(to: CGPoint(x: 22.587 * scale, y: 20.533 * scale))
            path.addLine(to: CGPoint(x: 18.48 * scale, y: 20.533 * scale))
            path.addLine(to: CGPoint(x: 18.48 * scale, y: 63.653 * scale))
            path.addLine(to: CGPoint(x: 22.587 * scale, y: 63.653 * scale))
            path.addLine(to: CGPoint(x: 22.587 * scale, y: 77.0 * scale))
            path.addLine(to: CGPoint(x: 0, y: 77.0 * scale))
            path.addLine(to: CGPoint(x: 0, y: 13.347 * scale))
            path.closeSubpath()

            context.fill(path, with: .color(color))
        }
        .frame(width: size, height: size)
    }
}

// MARK: - PermissionType — terse onboarding copy

private extension PermissionType {
    var terseLabel: String {
        switch self {
        case .location: "LOCATION"
        case .notifications: "NOTIFICATIONS"
        case .health: "HEALTH"
        case .microphone: "MICROPHONE"
        case .motion: "MOTION"
        case .camera: "CAMERA"
        case .photos: "PHOTOS"
        case .speechRecognition: "SPEECH"
        }
    }

    var terseExplanation: String {
        switch self {
        case .location: "Context and background sync signals."
        case .notifications: "Agent replies when backgrounded."
        case .health: "Read-only wellness signal for context."
        case .microphone: "Voice mode input stream."
        case .motion: "Activity signal for contextual awareness."
        case .camera: "Attach frames to the context."
        case .photos: "Organize and surface library items."
        case .speechRecognition: "On-device dictation in the composer."
        }
    }

    var requirementTag: String {
        switch self {
        case .location: "CONTEXT"
        case .notifications: "ASYNC TASKS"
        case .health: "WELLNESS"
        case .microphone: "VOICE MODE"
        case .motion: "ACTIVITY"
        case .camera: "VISION TOOLS"
        case .photos: "LIBRARY"
        case .speechRecognition: "DICTATION"
        }
    }
}
