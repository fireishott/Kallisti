import SwiftUI

struct SettingsScreen: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(HeraldHostStore.self) private var hostStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(PermissionsStore.self) private var permissionsStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(TabRouter.self) private var router
    @Environment(ThemeManager.self) private var themeManager

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            ScrollView {
                VStack(spacing: Design.Spacing.lg) {
                    connectionSection
                    relaySection
                    if settingsStore.availableEnvironments.count > 1 {
                        environmentSection
                    }
                    appearanceSection
                    preferencesSection
                    locationSection
                    privacySection
                    aboutSection
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.sm)
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { dismiss() } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: Design.Size.iconSmall, weight: .semibold))
                        .foregroundStyle(Design.Colors.foreground)
                }
            }
        }
        .task {
            await hostStore.refresh()
            await permissionsStore.reloadCapabilities()
        }
    }

    // MARK: - Connection

    private var connectionSection: some View {
        SettingsSectionView(title: "Connection") {
            VStack(spacing: 0) {
                settingsRow(
                    icon: sessionStore.state.connectionStatus.displayIcon,
                    iconColor: sessionStore.state.connectionStatus.displayColor,
                    title: "Status",
                    value: sessionStore.state.connectionStatus.displayLabel
                )

                sectionDivider

                if pairingStore.pairedRelayConfiguration != nil {
                    settingsNavRow(
                        icon: hostStatusRowIcon,
                        iconColor: hostStatusRowColor,
                        title: "Hermes Host",
                        value: hostStatusRowValue,
                        accessibilityIdentifier: "settings.heraldHost"
                    ) {
                        dismiss()
                        Task {
                            try? await Task.sleep(for: .milliseconds(300))
                            router.navigate(to: .connectHost)
                        }
                    }

                    sectionDivider
                }

                settingsToggle(
                    icon: "bolt.fill",
                    iconColor: Design.Colors.foreground,
                    title: "Auto-Connect",
                    isOn: autoConnectBinding
                )
            }
        }
    }

    // MARK: - Environment

    private var relaySection: some View {
        SettingsSectionView(title: "Relay") {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                if pairingStore.isPaired {
                    settingsRow(
                        icon: "point.3.connected.trianglepath.dotted",
                        iconColor: Design.Colors.foreground,
                        title: "Active Relay",
                        value: pairingStore.pairedRelayConfiguration?.hostDisplayName ?? relayConfiguration.relayOriginLabel
                    )
                    sectionDivider
                    settingsRow(
                        icon: "link",
                        iconColor: Design.Colors.secondaryForeground,
                        title: "Base URL",
                        value: pairingStore.pairedRelayConfiguration?.baseURLString ?? relayConfiguration.activeBaseURLString ?? "Not configured"
                    )
                    Text("Disconnect Herald before changing the relay configuration.")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .padding(.top, Design.Spacing.xs)
                } else {
                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        Text("CONNECTION MODE").brandEyebrow()

                        Picker("Connection Mode", selection: connectionModeBinding) {
                            ForEach(relayConfiguration.selectableConnectionModes, id: \.self) { mode in
                                Text(mode.compactLabel).tag(mode)
                            }
                        }
                        .pickerStyle(.segmented)
                    }

                    sectionDivider

                    if relayConfiguration.connectionMode.usesCustomRelayURL {
                        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                            TextField(customRelayURLPlaceholder, text: customRelayURLBinding)
                                .textInputAutocapitalization(.never)
                                .keyboardType(.URL)
                                .autocorrectionDisabled()
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)
                                .padding(Design.Spacing.md)
                                .background(Design.Colors.background)
                                .overlay(
                                    RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                                        .stroke(Design.Colors.border, lineWidth: 1)
                                )
                                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))

                            if let hint = relayConfiguration.connectionMode.relayURLHint {
                                Text(hint)
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                    .fixedSize(horizontal: false, vertical: true)
                            }

                            Text(relayConfiguration.connectionMode.shortDescription)
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    } else if let hostedRelayBaseURL = relayConfiguration.hostedRelayBaseURL {
                        settingsRow(
                            icon: "cloud",
                            iconColor: Design.Colors.foreground,
                            title: "Hosted Relay",
                            value: hostedRelayBaseURL
                        )
                    }

                    Text(backgroundDeliveryNote)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .fixedSize(horizontal: false, vertical: true)

                    if let relayValidationMessage {
                        Text(relayValidationMessage)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.warning)
                    }
                }
            }
        }
    }

    private var hostStatusRowIcon: String {
        switch hostStore.connectionState {
        case .online:
            return "desktopcomputer"
        case .offline:
            return "desktopcomputer.trianglebadge.exclamationmark"
        case .unreachable:
            return "wifi.exclamationmark"
        case .notConnected:
            return "desktopcomputer"
        }
    }

    private var hostStatusRowColor: Color {
        switch hostStore.connectionState {
        case .online:
            return Design.Colors.success
        case .offline, .unreachable:
            return Design.Colors.warning
        case .notConnected:
            return Design.Colors.secondaryForeground
        }
    }

    private var hostStatusRowValue: String {
        switch hostStore.connectionState {
        case .online, .offline:
            return hostStore.currentHost?.resolvedDisplayName ?? "Hermes Host"
        case .unreachable:
            return "Status unavailable"
        case .notConnected:
            return "Not Connected"
        }
    }

    private var environmentSection: some View {
        SettingsSectionView(title: "Internal Environment") {
            VStack(spacing: 0) {
                ForEach(Array(settingsStore.availableEnvironments.enumerated()), id: \.element) { index, env in
                    Button {
                        withAnimation(Design.Motion.quickResponse) {
                            settingsStore.settings.environment = env
                        }
                    } label: {
                        HStack {
                            Text(env.displayLabel)
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)

                            Spacer()

                            if settingsStore.settings.environment == env {
                                Image(systemName: "checkmark")
                                    .font(.system(size: 14, weight: .semibold))
                                    .foregroundStyle(Design.Brand.accent)
                            }
                        }
                        .frame(minHeight: Design.Size.minTapTarget)
                    }

                    if index < settingsStore.availableEnvironments.count - 1 {
                        sectionDivider
                    }
                }
            }
        }
    }

    // MARK: - Appearance

    private var appearanceSection: some View {
        SettingsSectionView(title: "Appearance") {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                // Theme preset picker
                VStack(alignment: .leading, spacing: 8) {
                    Text("Theme")
                        .font(Design.Typography.footnote)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 12) {
                            ForEach(ThemePreset.allCases) { theme in
                                themeSwatch(theme)
                            }
                        }
                    }
                }

                Divider()
                    .overlay(Design.Colors.divider)

                // Light/Dark/System toggle
                VStack(alignment: .leading, spacing: 8) {
                    Text("Appearance")
                        .font(Design.Typography.footnote)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Picker("Appearance", selection: colorSchemePreferenceBinding) {
                        ForEach(ColorSchemePreference.allCases) { pref in
                            Text(pref.label).tag(pref)
                        }
                    }
                    .pickerStyle(.segmented)
                }

                Divider()
                    .overlay(Design.Colors.divider)

                // Chat wallpaper entry point
                NavigationLink {
                    WallpaperPickerSheet()
                } label: {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "photo.fill")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.accent)
                            .frame(width: 20, alignment: .center)

                        Text("Chat Wallpaper")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)

                        Spacer()

                        Text(settingsStore.settings.chatWallpaper.label)
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)

                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func themeSwatch(_ theme: ThemePreset) -> some View {
        Button {
            withAnimation(Design.Motion.quickResponse) {
                themeManager.preset = theme
            }
            settingsStore.settings.themePreset = theme
        } label: {
            VStack(spacing: 4) {
                Circle()
                    .fill(theme.accent)
                    .frame(width: 32, height: 32)
                    .overlay(
                        Circle()
                            .strokeBorder(
                                themeManager.preset == theme ? Color.white : Color.clear,
                                lineWidth: 2
                            )
                    )
                Text(theme.label)
                    .font(.caption2)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
        .buttonStyle(.plain)
    }

    private var colorSchemePreferenceBinding: Binding<ColorSchemePreference> {
        Binding(
            get: { themeManager.colorSchemePreference },
            set: { newValue in
                themeManager.colorSchemePreference = newValue
                settingsStore.settings.colorSchemePreference = newValue
            }
        )
    }

    // MARK: - Preferences

    private var preferencesSection: some View {
        SettingsSectionView(title: "Preferences") {
            VStack(spacing: 0) {
                settingsToggle(
                    icon: "bell.fill",
                    iconColor: Design.Colors.foreground,
                    title: "Notifications",
                    isOn: notificationsBinding
                )

                sectionDivider

                settingsToggle(
                    icon: "hand.tap.fill",
                    iconColor: Design.Colors.foreground,
                    title: "Haptic Feedback",
                    isOn: hapticBinding
                )
            }
        }
    }

    // MARK: - Location

    private var locationSection: some View {
        SettingsSectionView(title: "Location") {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                settingsRow(
                    icon: "location.fill",
                    iconColor: Design.Brand.primary,
                    title: "Authorization",
                    value: permissionsStore.locationAuthorizationLevel.displayLabel
                )

                sectionDivider

                settingsRow(
                    icon: "scope",
                    iconColor: Design.Brand.primary,
                    title: "Accuracy",
                    value: permissionsStore.locationAccuracyLevel.displayLabel
                )

                sectionDivider

                settingsToggle(
                    icon: "location.circle.fill",
                    iconColor: Design.Brand.primary,
                    title: "Background Location",
                    isOn: backgroundLocationBinding
                )

                Text(backgroundLocationDescription)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
    }

    // MARK: - Privacy

    private var privacySection: some View {
        SettingsSectionView(title: "Privacy") {
            settingsNavRow(
                icon: "lock.shield.fill",
                iconColor: Design.Colors.success,
                title: "Permissions"
            ) {
                dismiss()
                Task {
                    try? await Task.sleep(for: .milliseconds(300))
                    router.navigate(to: .permissions)
                }
            }
        }
    }

    // MARK: - About

    private var aboutSection: some View {
        SettingsSectionView(title: "About") {
            VStack(spacing: 0) {
                settingsRow(
                    icon: "info.circle",
                    iconColor: Design.Colors.secondaryForeground,
                    title: "Version",
                    value: "\(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0") (\(Bundle.main.object(forInfoDictionaryKey: kCFBundleVersionKey as String) as? String ?? "1"))"
                )

                sectionDivider

                settingsNavRow(
                    icon: "doc.text",
                    iconColor: Design.Colors.secondaryForeground,
                    title: "Terms of Service"
                ) {
                    openConfiguredURL(settingsStore.buildConfiguration.termsOfServiceURL)
                }

                sectionDivider

                settingsNavRow(
                    icon: "hand.raised",
                    iconColor: Design.Colors.secondaryForeground,
                    title: "Privacy Policy"
                ) {
                    openConfiguredURL(settingsStore.buildConfiguration.privacyPolicyURL)
                }

                if settingsStore.buildConfiguration.supportURL != nil {
                    sectionDivider

                    settingsNavRow(
                        icon: "questionmark.circle",
                        iconColor: Design.Colors.secondaryForeground,
                        title: "Support"
                    ) {
                        openConfiguredURL(settingsStore.buildConfiguration.supportURL)
                    }
                }
            }
        }
    }

    // MARK: - Bindings

    private var autoConnectBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.autoConnectOnLaunch },
            set: { settingsStore.settings.autoConnectOnLaunch = $0 }
        )
    }

    private var notificationsBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.notificationsEnabled },
            set: { newValue in
                settingsStore.settings.notificationsEnabled = newValue
                // Immediately register or deactivate push token on the relay
                Task {
                    await AppContainer.sharedDefault().reregisterStoredPushToken()
                }
            }
        )
    }

    private var hapticBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.hapticFeedbackEnabled },
            set: { settingsStore.settings.hapticFeedbackEnabled = $0 }
        )
    }

    private var backgroundLocationBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.locationSyncPreference == .backgroundAllowed },
            set: { isEnabled in
                let preference: LocationSyncPreference = isEnabled ? .backgroundAllowed : .foregroundOnly
                settingsStore.settings.locationSyncPreference = preference
                permissionsStore.updateLocationSyncPreference(preference)

                guard isEnabled else { return }

                Task {
                    switch permissionsStore.locationAuthorizationLevel {
                    case .denied, .restricted:
                        permissionsStore.openLocationSystemSettings()
                    case .always, .whenInUse:
                        // Both levels support CLBackgroundActivitySession.
                        // While In Use shows blue indicator; Always does not.
                        await permissionsStore.requestBackgroundLocationAccess()
                    case .notDetermined:
                        await permissionsStore.requestBackgroundLocationAccess()
                    }
                }
            }
        )
    }

    private var relayConfiguration: RelayConfiguration {
        settingsStore.settings.relayConfiguration
    }

    private var relayValidationMessage: String? {
        relayConfiguration.validationMessage
    }

    private var customRelayURLPlaceholder: String {
        switch relayConfiguration.connectionMode {
        case .managedRelay:
            return "https://your-relay.example.com/v1"
        case .tailscale:
            return "https://my-mac.tail-scale.ts.net/v1"
        case .selfHostedRelay:
            return "https://your-relay.example.com/v1"
        }
    }

    private var backgroundDeliveryNote: String {
        let mode = relayConfiguration.connectionMode
        if mode == .managedRelay && !settingsStore.buildConfiguration.usesManagedPushBroker {
            return "Managed mode selected, but this build uses direct relay push only."
        }
        return mode.backgroundDeliveryNote
    }

    private var backgroundLocationDescription: String {
        if settingsStore.settings.locationSyncPreference == .backgroundAllowed {
            switch permissionsStore.locationAuthorizationLevel {
            case .always:
                return "Herald receives location updates in the background without the blue indicator."
            case .whenInUse:
                return "Herald receives background location updates. A blue indicator appears at the top of the screen when active."
            case .notDetermined:
                return "Enabling this will request location access so Herald can sync while backgrounded."
            case .denied, .restricted:
                return "Location is blocked at the system level. Open Settings to allow Herald to request background updates."
            }
        }

        return "Foreground-only keeps location updates limited to active app use."
    }

    private var connectionModeBinding: Binding<RelayConnectionMode> {
        Binding(
            get: { settingsStore.settings.relayConfiguration.connectionMode },
            set: { newValue in
                var relayConfiguration = settingsStore.settings.relayConfiguration
                relayConfiguration.updateConnectionMode(newValue)
                settingsStore.settings.relayConfiguration = relayConfiguration
            }
        )
    }

    private var customRelayURLBinding: Binding<String> {
        Binding(
            get: { settingsStore.settings.relayConfiguration.customRelayBaseURL },
            set: { newValue in
                var relayConfiguration = settingsStore.settings.relayConfiguration
                relayConfiguration.customRelayBaseURL = newValue.trimmingCharacters(in: .whitespacesAndNewlines)
                settingsStore.settings.relayConfiguration = relayConfiguration
            }
        )
    }

    // MARK: - Row Components

    private var sectionDivider: some View {
        Divider()
            .overlay(Design.Colors.divider)
    }

    private func settingsRow(icon: String, iconColor: Color, title: String, value: String?) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: icon)
                .font(.system(size: 14))
                .foregroundStyle(iconColor)
                .frame(width: 20, alignment: .center)

            Text(title)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)

            Spacer()

            if let value {
                Text(value)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    @ViewBuilder
    private func settingsNavRow(
        icon: String,
        iconColor: Color,
        title: String,
        value: String? = nil,
        accessibilityIdentifier: String? = nil,
        action: @escaping () -> Void
    ) -> some View {
        let row = Button(action: action) {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: icon)
                    .font(.system(size: 14))
                    .foregroundStyle(iconColor)
                    .frame(width: 20, alignment: .center)

                Text(title)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)

                Spacer()

                if let value {
                    Text(value)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }

                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            .frame(minHeight: Design.Size.minTapTarget)
        }

        if let accessibilityIdentifier {
            row.accessibilityIdentifier(accessibilityIdentifier)
        } else {
            row
        }
    }

    private func settingsToggle(
        icon: String,
        iconColor: Color,
        title: String,
        isOn: Binding<Bool>
    ) -> some View {
        Toggle(isOn: isOn) {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: icon)
                    .font(.system(size: 14))
                    .foregroundStyle(iconColor)
                    .frame(width: 20, alignment: .center)

                Text(title)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
            }
        }
        .tint(Design.Brand.accent)
        .frame(minHeight: Design.Size.minTapTarget)
    }

    private func openConfiguredURL(_ url: URL?) {
        guard let url else { return }
        openURL(url)
    }
}
