import SwiftUI

/// The Daemon entry on the Settings page, and the six rows under it.
///
/// This is a card, not a section of controls: the controls are on the group
/// screens, one per group the daemon publishes. A row states the group, its symbol
/// and how many settings it holds, and a live count means the card says something
/// true before it is opened.
struct DaemonSettingsSection: View {
    @Environment(DaemonSettingsStore.self) private var store

    var body: some View {
        SettingsSectionView(title: "Daemon") {
            ForEach(DaemonGroup.allCases) { group in
                NavigationLink(value: Route.daemonGroup(group)) {
                    DaemonGroupLabel(group: group, count: store.count(in: group))
                }
                .buttonStyle(.plain)
                if group != DaemonGroup.allCases.last {
                    Divider().opacity(0.35)
                }
            }
        }
        .task {
            if store.settings.isEmpty { await store.load() }
        }
    }
}

/// One group row: symbol, title, live count, chevron.
struct DaemonGroupLabel: View {
    let group: DaemonGroup
    let count: Int

    var body: some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: group.symbol)
                .font(.system(size: 15, weight: .regular))
                .foregroundStyle(Design.Colors.accent)
                .frame(width: 24)
            Text(group.title)
                .font(Design.Typography.body)
                .foregroundStyle(Design.Colors.foreground)
            Spacer(minLength: Design.Spacing.xs)
            Text(count == 1 ? "1" : "\(count)")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.tertiaryForeground)
            Image(systemName: "chevron.right")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Design.Colors.tertiaryForeground)
        }
        .frame(minHeight: 44)
        .contentShape(Rectangle())
    }
}
