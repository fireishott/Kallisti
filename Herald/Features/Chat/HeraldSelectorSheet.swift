import SwiftUI

/// Unified sheet for switching models and profiles.
///
/// Combines what was previously two separate sheets (ModelSelectorSheet and
/// ProfileSelectorSheet) into one tabbed experience. The Models tab shows the
/// full model catalog grouped by provider; the Profiles tab shows the Hermes
/// profile tree with rich metadata inspired by the Hermes CLI `profiles.py`.
///
/// Tapping a model switches it via `ModelStore.switchModel(to:provider:)`.
/// Tapping a profile switches it by sending `/profile <name>` through chat.
struct HeraldSelectorSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(ModelStore.self) private var modelStore
    @Environment(ChatStore.self) private var chatStore
    @Environment(ProfileStore.self) private var profileStore
    @Environment(KallistiHostStore.self) private var hostStore
    @Environment(SkillsStore.self) private var skillsStore

    enum Tab: String, CaseIterable {
        case models
        case profiles
        case skills

        var label: String {
            switch self {
            case .models: return "Models"
            case .profiles: return "Profiles"
            case .skills: return "Skills"
            }
        }

        var icon: String {
            switch self {
            case .models: return "cpu"
            case .profiles: return "brain.head.profile"
            case .skills: return "wrench.and.screwdriver"
            }
        }
    }

    @State private var selectedTab: Tab
    @State private var searchText = ""
    @State private var setAsGlobalDefault = false
    @State private var isSwitching = false
    @State private var switchingModelID: String?
    @State private var switchingProfileName: String?
    @State private var switchError: String?

    init(initialTab: Tab = .models) {
        self._selectedTab = State(initialValue: initialTab)
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                // Active state summary card
                activeStateCard
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.top, Design.Spacing.sm)

                // Error banner
                if let switchError {
                    errorBanner(message: switchError)
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.top, Design.Spacing.sm)
                }

                // Tab picker
                Picker("Tab", selection: $selectedTab) {
                    ForEach(Tab.allCases, id: \.self) { tab in
                        Label(tab.label, systemImage: tab.icon).tag(tab)
                    }
                }
                .pickerStyle(.segmented)
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.sm)

                // Search field
                HStack(spacing: Design.Spacing.xs) {
                    Image(systemName: "magnifyingglass")
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    TextField("Filter models or providers...", text: $searchText)
                        .textFieldStyle(.plain)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                    if !searchText.isEmpty {
                        Button {
                            searchText = ""
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    }
                }
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.vertical, Design.Spacing.xs)
                .background(Design.Colors.surface)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                .overlay(
                    RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                        .stroke(Design.Colors.border, lineWidth: 1)
                )
                .padding(.horizontal, Design.Spacing.md)
                .padding(.bottom, Design.Spacing.xs)

                // Content
                Group {
                    switch selectedTab {
                    case .models:
                        modelList
                    case .profiles:
                        profileList
                    case .skills:
                        skillsList
                    }
                }
            }
            .background(Design.Colors.background)
            .navigationTitle("Kallisti Hub")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                // Build 128.99: "Set as default" toggle moved from the
                // bottom of the Models list to the top-left, opposite the
                // Done button, per Curtis.
                ToolbarItem(placement: .topBarLeading) {
                    Toggle(isOn: $setAsGlobalDefault) {
                        Text("Set as default")
                            .font(Design.Typography.caption)
                    }
                    .tint(Design.Brand.accent)
                    .accessibilityHint("Persist beyond the current session (--global)")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .task {
            await modelStore.loadModels(force: true)
            await profileStore.loadProfiles(force: true)
        }
    }

    // MARK: - Active State Card

    private var activeStateCard: some View {
        HStack(spacing: Design.Spacing.md) {
            // Profile info
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Circle()
                        .fill(hostStore.isHostOnline ? Design.Colors.success : Design.Colors.warning)
                        .frame(width: 6, height: 6)
                    Text(profileStore.displayProfileName)
                        .font(Design.Typography.headline)
                        .foregroundStyle(Design.Colors.foreground)
                }
                if let profile = profileStore.activeProfile {
                    Text("\(profile.skillCount) skills")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            }

            Spacer()

            // Divider
            Rectangle()
                .fill(Design.Colors.divider)
                .frame(width: 1, height: 28)

            // Model info
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Image(systemName: "cpu")
                        .font(.system(size: 10))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Text(modelStore.activeModel?.name ?? chatStore.activeModelName ?? "—")
                        .font(Design.Typography.headline)
                        .foregroundStyle(Design.Colors.foreground)
                        .lineLimit(1)
                }
                if let ctx = modelStore.activeModel?.contextWindow {
                    Text(formatTokenCount(ctx) + " context")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            }
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
    }

    // MARK: - Models Tab

    private var modelList: some View {
        Group {
            if modelStore.isLoading && modelStore.models.isEmpty {
                ProgressView("Loading models…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if modelStore.models.isEmpty {
                emptyState(
                    icon: "cpu",
                    message: modelStore.errorMessage ?? "No models available",
                    hint: "Model list comes from the Hermes host — make sure it's online."
                )
            } else {
                List {
                    ForEach(filteredModelGroups, id: \.provider) { group in
                        Section(group.provider) {
                            ForEach(group.models) { model in
                                modelRow(model)
                            }
                        }
                    }
                }
                .scrollContentBackground(.hidden)
                .refreshable {
                    await modelStore.loadModels(force: true)
                }
            }
        }
    }

    private func modelRow(_ model: ModelStore.HeraldModel) -> some View {
        Button {
            selectModel(model)
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                VStack(alignment: .leading, spacing: 2) {
                    // Top row: humanized model name (e.g. "DeepSeek V4 Pro").
                    Text(ModelNamePretty.prettyName(model.name))
                        .font(.system(.callout, weight: .semibold))
                        .foregroundStyle(Design.Colors.foreground)
                        .lineLimit(1)

                    // Bottom row: "<provider>/<family>" provenance line
                    // (e.g. "9Router/DeepSeek"). Falls back to the slug
                    // prefix in model.name when no provider info is set.
                    Text(proxyLine(for: model))
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .lineLimit(1)

                    if let contextWindow = model.contextWindow {
                        Text("\(formatTokenCount(contextWindow)) context")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                    } else if model.isProviderDefault == true {
                        Text("provider default")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                    }
                }

                Spacer()

                if isSwitching && switchingModelID == model.id {
                    ProgressView()
                } else if modelStore.isActive(model) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(Design.Brand.accent)
                        .font(.system(size: 18))
                } else {
                    Image(systemName: "circle")
                        .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.3))
                        .font(.system(size: 18))
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(isSwitching)
        .listRowBackground(
            modelStore.isActive(model)
                ? Design.Brand.accent.opacity(0.08)
                : Color.clear
        )
    }

    // MARK: - Profiles Tab

    private var profileList: some View {
        Group {
            if profileStore.isLoading && profileStore.profiles.isEmpty {
                ProgressView("Loading profiles…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if profileStore.profiles.isEmpty {
                emptyState(
                    icon: "brain.head.profile",
                    message: profileStore.errorMessage ?? "No profiles available",
                    hint: "Profiles are configured on the Hermes host. Create one with 'hermes profile create <name>'."
                )
            } else {
                List {
                    ForEach(profileStore.profiles) { profile in
                        Button {
                            selectProfile(profile)
                        } label: {
                            HStack(spacing: Design.Spacing.sm) {
                                // Profile icon with active indicator
                                ZStack {
                                    Circle()
                                        .fill(profile.name == profileStore.activeProfileName
                                            ? Design.Brand.accent.opacity(0.15)
                                            : Design.Colors.surface)
                                        .frame(width: 36, height: 36)

                                    Image(systemName: "brain.head.profile")
                                        .font(.system(size: 14))
                                        .foregroundStyle(
                                            profile.name == profileStore.activeProfileName
                                                ? Design.Brand.accent
                                                : Design.Colors.secondaryForeground
                                        )
                                }

                                VStack(alignment: .leading, spacing: 2) {
                                    HStack(spacing: 4) {
                                        Text(profile.name)
                                            .font(.system(.callout, weight: .semibold))
                                            .foregroundStyle(Design.Colors.foreground)

                                        if profile.name == profileStore.activeProfileName {
                                            Text("ACTIVE")
                                                .font(.system(size: 8, weight: .bold))
                                                .foregroundStyle(Design.Brand.accent)
                                                .padding(.horizontal, 4)
                                                .padding(.vertical, 1)
                                                .background(Design.Brand.accent.opacity(0.12))
                                                .clipShape(Capsule())
                                        }
                                    }

                                    if !profile.description.isEmpty {
                                        Text(profile.description)
                                            .font(Design.Typography.caption)
                                            .foregroundStyle(Design.Colors.secondaryForeground)
                                            .lineLimit(2)
                                    }

                                    HStack(spacing: Design.Spacing.sm) {
                                        Label("\(profile.skillCount) skills", systemImage: "hammer")
                                            .font(.system(size: 10))
                                            .foregroundStyle(Design.Colors.tertiaryForeground)
                                    }
                                }

                                Spacer()

                                if isSwitching && switchingProfileName == profile.name {
                                    ProgressView()
                                } else if profile.name == profileStore.activeProfileName {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundStyle(Design.Brand.accent)
                                        .font(.system(size: 18))
                                } else {
                                    Image(systemName: "arrow.right.circle")
                                        .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.3))
                                        .font(.system(size: 18))
                                }
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(isSwitching)
                        .listRowBackground(
                            profile.name == profileStore.activeProfileName
                                ? Design.Brand.accent.opacity(0.06)
                                : Color.clear
                        )
                    }
                }
                .scrollContentBackground(.hidden)
                .refreshable {
                    await profileStore.loadProfiles(force: true)
                }
            }
        }
    }

    // MARK: - Skills Tab (Build 128.50)

    /// Embeds the same SkillsBrowserView the iPad sidebar uses, so the hub
    /// shows the full available skill list with search + detail navigation.
    /// The sheet's task also force-loads the catalog so the count in the
    /// active state card and this list stay in sync.
    @ViewBuilder
    private var skillsList: some View {
        SkillsBrowserView()
            .task {
                await skillsStore.loadSkills(force: true)
            }
    }

    // MARK: - Actions


    private var filteredModelGroups: [(provider: String, models: [ModelStore.HeraldModel])] {
        let trimmed = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !trimmed.isEmpty else { return modelStore.modelsByProvider }
        return modelStore.modelsByProvider.compactMap { group in
            let matchingModels = group.models.filter { model in
                model.name.lowercased().contains(trimmed) ||
                group.provider.lowercased().contains(trimmed) ||
                model.id.lowercased().contains(trimmed)
            }
            if matchingModels.isEmpty { return nil }
            return (provider: group.provider, models: matchingModels)
        }
    }

    private func selectModel(_ model: ModelStore.HeraldModel) {
        switchError = nil
        isSwitching = true
        switchingModelID = model.id
        Task {
            do {
                // Build 60: race the switch against a 35s timeout so the
                // spinner never hangs forever on a phantom socket.
                try await withThrowingTaskGroup(of: String.self) { group in
                    group.addTask {
                        try await modelStore.switchModel(to: model.name, provider: model.provider, global: setAsGlobalDefault)
                        return "done"
                    }
                    group.addTask {
                        try await Task.sleep(for: .seconds(35))
                        throw CancellationError()
                    }
                    let _ = try await group.next()!
                    group.cancelAll()
                }
                isSwitching = false
                // --global is now threaded through switchModel's slash.exec
                // command (see ModelStore.swift / NativeGatewayFeatureClient),
                // so there is no separate chat-message path to send here. The
                // gateway persists the switch on the same slash.exec call.
                dismiss()
            } catch {
                isSwitching = false
                switchError = error.localizedDescription
            }
        }
    }

    private func selectProfile(_ profile: ProfileStore.HeraldProfile) {
        switchError = nil
        isSwitching = true
        switchingProfileName = profile.name
        Task {
            do {
                try await profileStore.switchProfile(to: profile.name)
            } catch {
                switchError = error.localizedDescription
            }
            isSwitching = false
            dismiss()
        }
    }

    // MARK: - Helpers

    /// Builds the secondary provenance line under a model row, in the form
    /// "<provider>/<family>" (e.g. "9Router/DeepSeek"). Provider uses the
    /// model's display name when available, falling back to the slug prefix
    /// in `model.name` when no provider info is set. Family comes from
    /// `ModelNamePretty.familyName` so the brand token matches the top row.
    private func proxyLine(for model: ModelStore.HeraldModel) -> String {
        let provider: String
        if let display = model.providerName, !display.isEmpty {
            provider = display
        } else if !model.provider.isEmpty {
            provider = model.provider
        } else if let prefix = model.name.split(separator: "/").first {
            provider = String(prefix)
        } else {
            provider = "Unknown"
        }
        let family = ModelNamePretty.familyName(model.name)
        return "\(provider)/\(family)"
    }

    private func errorBanner(message: String) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            Text(message)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.foreground)
                .lineLimit(2)
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
    }

    private func emptyState(icon: String, message: String, hint: String) -> some View {
        VStack(spacing: Design.Spacing.md) {
            Image(systemName: icon)
                .font(.system(size: 32))
                .foregroundStyle(Design.Colors.secondaryForeground)
            Text(message)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)
            Text(hint)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)

            Button("Retry") {
                Task {
                    await modelStore.loadModels(force: true)
                    await profileStore.loadProfiles(force: true)
                }
            }
            .buttonStyle(.bordered)
        }
        .padding(Design.Spacing.lg)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func formatTokenCount(_ count: Int) -> String {
        if count >= 1_000_000 {
            return String(format: "%.1fM", Double(count) / 1_000_000)
                .replacingOccurrences(of: ".0M", with: "M")
        } else if count >= 1_000 {
            return "\(count / 1_000)K"
        }
        return "\(count)"
    }
}
