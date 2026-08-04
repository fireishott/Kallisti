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

    enum Tab: String, CaseIterable {
        case models
        case profiles

        var label: String {
            switch self {
            case .models: return "Models"
            case .profiles: return "Profiles"
            }
        }

        var icon: String {
            switch self {
            case .models: return "cpu"
            case .profiles: return "brain.head.profile"
            }
        }
    }

    @State private var selectedTab: Tab
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

                // Content
                Group {
                    switch selectedTab {
                    case .models:
                        modelList
                    case .profiles:
                        profileList
                    }
                }
            }
            .background(Design.Colors.background)
            .navigationTitle("Kallisti Hub")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
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
                    Text(profileStore.activeProfileName ?? "default")
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
                    ForEach(modelStore.modelsByProvider, id: \.provider) { group in
                        Section(group.provider) {
                            ForEach(group.models) { model in
                                modelRow(model)
                            }
                        }
                    }

                    Section {
                        Toggle(isOn: $setAsGlobalDefault) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Set as default")
                                    .font(Design.Typography.callout)
                                Text("Persist beyond the current session (--global)")
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                            }
                        }
                        .tint(Design.Brand.accent)
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
                    Text(model.name)
                        .font(.system(.callout, design: .monospaced, weight: .medium))
                        .foregroundStyle(Design.Colors.foreground)
                        .lineLimit(1)

                    HStack(spacing: 6) {
                        if let contextWindow = model.contextWindow {
                            Text("\(formatTokenCount(contextWindow)) context")
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                        if model.isProviderDefault == true {
                            Text("provider default")
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
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

    // MARK: - Actions

    private func selectModel(_ model: ModelStore.HeraldModel) {
        switchError = nil
        isSwitching = true
        switchingModelID = model.id
        Task {
            do {
                try await modelStore.switchModel(to: model.name, provider: model.provider)
                isSwitching = false
                // If --global was toggled on, send /model as a chat message
                if setAsGlobalDefault {
                    await chatStore.sendMessage("/model \(model.name) --global")
                }
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
