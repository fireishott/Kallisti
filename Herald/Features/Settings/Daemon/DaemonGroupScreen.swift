import SwiftUI

/// One daemon group's screen: hero, then the fields it holds as glass cards.
///
/// Every field comes from the record. The screen knows the group's title, blurb and
/// symbol and nothing else, which is what makes a field added to the daemon appear
/// here with no client change.
struct DaemonGroupScreen: View {
    let group: DaemonGroup
    @Environment(DaemonSettingsStore.self) private var store
    @State private var drafts: [String: String] = [:]
    @State private var filter = ""
    @State private var savedTick = 0

    private var fields: [DaemonSetting] {
        let all = store.fields(in: group)
        guard !filter.isEmpty else { return all }
        return all.filter { $0.label.localizedCaseInsensitiveContains(filter) }
    }

    var body: some View {
        ZStack {
            background
            ScrollView {
                VStack(alignment: .leading, spacing: Design.Spacing.lg) {
                    header
                    content
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.sm)
            }
        }
        .navigationTitle(group.title)
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $filter, prompt: "Filter these settings")
        .navigationTransitionIfAvailable()
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                if !store.dirty.isEmpty {
                    Button("Save") {
                        Task {
                            await store.commit()
                            savedTick += 1
                        }
                    }
                    .fontWeight(.semibold)
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                if store.justSaved {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(Design.Colors.accent)
                        .symbolEffect(.bounce, value: savedTick)
                }
            }
        }
        .task {
            if store.settings.isEmpty { await store.load() }
            seedDrafts()
        }
        .onChange(of: store.settings) { _, _ in seedDrafts() }
    }

    // MARK: - Pieces

    private var background: some View {
        ZStack(alignment: .topLeading) {
            Design.Colors.background.ignoresSafeArea()
            RadialGradient(
                colors: [Design.Colors.accent.opacity(0.12), .clear],
                center: .topLeading,
                startRadius: 0,
                endRadius: 420
            )
            .ignoresSafeArea()
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: group.symbol)
                    .font(.system(size: 20, weight: .regular))
                    .foregroundStyle(Design.Colors.accent)
                Text("Daemon")
                    .brandEyebrow()
            }
            Text(group.title)
                .font(Design.Typography.screenTitle)
                .foregroundStyle(Design.Colors.foreground)
            Text(group.blurb)
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .fixedSize(horizontal: false, vertical: true)
            Text(pluralised(store.count(in: group)))
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.tertiaryForeground)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private var content: some View {
        if store.needsAdmin {
            notice("This phone was paired without administration, so the daemon refuses the settings read. Mint a pairing code with the administration box ticked and pair again.")
        } else if let error = store.loadError {
            notice(error)
        } else if store.isLoading {
            VStack(spacing: Design.Spacing.sm) {
                ForEach(0..<4, id: \.self) { _ in
                    RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                        .fill(Design.Colors.surface)
                        .frame(height: 44)
                }
            }
        } else if fields.isEmpty {
            notice("No setting in this group.")
        } else {
            VStack(spacing: 0) {
                ForEach(Array(fields.enumerated()), id: \.element.id) { index, setting in
                    DaemonFieldRow(
                        setting: setting,
                        refusal: store.refusal[setting.key],
                        draft: draftBinding(for: setting),
                        onBool: { on in
                            drafts[setting.key] = on ? "true" : "false"
                            Task {
                                await store.setBool(setting.key, to: on)
                                savedTick += 1
                            }
                        },
                        onDirty: { markDirty(setting.key) }
                    )
                    if index != fields.count - 1 {
                        Divider().opacity(0.35)
                    }
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .glassCard()
        }
    }

    private func notice(_ text: String) -> some View {
        Text(text)
            .font(Design.Typography.footnote)
            .foregroundStyle(Design.Colors.secondaryForeground)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Design.Spacing.md)
            .glassCard()
    }

    private func pluralised(_ count: Int) -> String {
        count == 1 ? "1 setting" : "\(count) settings"
    }

    // MARK: - Drafts

    private func draftBinding(for setting: DaemonSetting) -> Binding<String> {
        Binding(
            get: { drafts[setting.key] ?? setting.value },
            set: { drafts[setting.key] = $0 }
        )
    }

    private func markDirty(_ key: String) {
        guard let text = drafts[key] else { return }
        _ = store.set(key, to: text)
    }

    private func seedDrafts() {
        for setting in store.settings where drafts[setting.key] == nil {
            drafts[setting.key] = setting.value
        }
    }
}

/// A hairline: a glass card when the platform has it, the same surface and stroke
/// `SettingsSectionView` uses when it does not.
extension View {
    @ViewBuilder
    func glassCard() -> some View {
        if #available(iOS 26.0, *) {
            self.adaptiveGlass(in: .rect(cornerRadius: 22))
        } else {
            self
                .background(Design.Colors.surface)
                .overlay(
                    RoundedRectangle(cornerRadius: 22)
                        .stroke(Design.Colors.border, lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: 22))
        }
    }

    @ViewBuilder
    func navigationTransitionIfAvailable() -> some View {
        if #available(iOS 18.0, *) {
            self.navigationTransition(.zoom)
        } else {
            self
        }
    }
}
