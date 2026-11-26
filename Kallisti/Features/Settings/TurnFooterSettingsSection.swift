import SwiftUI

/// The "Turn footer" block on the Settings page.
///
/// This is built from the same `SettingsSectionView` shell, the same eyebrow row
/// and the same switch indicator every other settings section uses. It used to be
/// a bare SwiftUI `Section` of stock `Toggle`s, which on this page meant a
/// different header font, a different card background, a different row height and
/// a stock switch beside hand-drawn ones - the one section on the screen that
/// looked like it came from another app.
///
/// The field switches hide under the master switch. A footer the operator turned
/// off must not leave twelve live switches behind it that appear to do nothing.
struct TurnFooterSettingsSection: View {
    @AppStorage(TurnFooterPrefs.enabled) private var isEnabled: Bool = true
    @AppStorage(TurnFooterPrefs.model) private var showsModel: Bool = true
    @AppStorage(TurnFooterPrefs.provider) private var showsProvider: Bool = true
    @AppStorage(TurnFooterPrefs.rounds) private var showsRounds: Bool = true
    @AppStorage(TurnFooterPrefs.duration) private var showsDuration: Bool = true
    @AppStorage(TurnFooterPrefs.tokens) private var showsTokens: Bool = true
    @AppStorage(TurnFooterPrefs.cache) private var showsCache: Bool = true
    @AppStorage(TurnFooterPrefs.context) private var showsContext: Bool = false
    @AppStorage(TurnFooterPrefs.cost) private var showsCost: Bool = true
    @AppStorage(TurnFooterPrefs.sessionCost) private var showsSessionCost: Bool = true
    @AppStorage(TurnFooterPrefs.turns) private var showsTurns: Bool = true
    @AppStorage(TurnFooterPrefs.tools) private var showsTools: Bool = true
    @AppStorage(TurnFooterPrefs.skills) private var showsSkills: Bool = true

    var body: some View {
        SettingsSectionView(title: "Turn footer") {
            VStack(spacing: 0) {
                row(
                    icon: "text.alignleft",
                    title: "Show turn footer",
                    subtitle: "One line under a finished answer",
                    isOn: $isEnabled
                )

                if isEnabled {
                    divider
                    row(
                        icon: "cpu",
                        title: "Model",
                        subtitle: "Which model answered",
                        isOn: $showsModel
                    )
                    divider
                    row(
                        icon: "network",
                        title: "Provider",
                        subtitle: "Which connection served it",
                        isOn: $showsProvider
                    )
                    divider
                    row(
                        icon: "arrow.triangle.2.circlepath",
                        title: "Tool rounds",
                        subtitle: "How many passes the turn took",
                        isOn: $showsRounds
                    )
                    divider
                    row(
                        icon: "wrench.and.screwdriver",
                        title: "Tool calls",
                        subtitle: "How many tools ran",
                        isOn: $showsTools
                    )
                    divider
                    row(
                        icon: "books.vertical",
                        title: "Skills used",
                        subtitle: "How many of those were skill reads",
                        isOn: $showsSkills
                    )
                    divider
                    row(
                        icon: "clock",
                        title: "Wall time",
                        subtitle: "Request to last byte, as an estimate",
                        isOn: $showsDuration
                    )
                    divider
                    row(
                        icon: "number",
                        title: "Tokens in / out",
                        subtitle: "Prompt and completion counts",
                        isOn: $showsTokens
                    )
                    divider
                    row(
                        icon: "arrow.down.to.line",
                        title: "Cached tokens",
                        subtitle: "Prompt tokens served from cache",
                        isOn: $showsCache
                    )
                    divider
                    row(
                        icon: "dollarsign.circle",
                        title: "Turn cost",
                        subtitle: "Needs a price for the model in the daemon",
                        isOn: $showsCost
                    )
                    divider
                    row(
                        icon: "chart.line.uptrend.xyaxis",
                        title: "Session cost",
                        subtitle: "Running total for this chat",
                        isOn: $showsSessionCost
                    )
                    divider
                    row(
                        icon: "bubble.left.and.bubble.right",
                        title: "Turn counter",
                        subtitle: "How many turns deep this chat is",
                        isOn: $showsTurns
                    )
                    divider
                    row(
                        icon: "rectangle.compress.vertical",
                        title: "Context used",
                        subtitle: "How full the window is, against its declared size",
                        isOn: $showsContext
                    )
                }
            }

            if isEnabled {
                Text("A field with nothing measured is left out, never shown as a zero. Cost appears only when the daemon has a price for the model.")
                    .font(Design.Typography.caption2)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, Design.Spacing.sm)
            }
        }
    }

    /// The same hairline the other sections separate their rows with.
    private var divider: some View {
        Rectangle()
            .fill(Design.Colors.divider)
            .frame(height: 0.5)
    }

    /// One switch row, matching `SettingsScreen.settingsToggle` exactly: same
    /// icon column, same type ramp, same hand-drawn capsule, same full-row tap
    /// target. Duplicated here rather than reached for across files because that
    /// helper is private to the settings screen, and making it public to serve
    /// one section would put the page's layout in two places again.
    private func row(
        icon: String,
        title: String,
        subtitle: String?,
        isOn: Binding<Bool>
    ) -> some View {
        Button {
            isOn.wrappedValue.toggle()
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: icon)
                    .font(.system(size: 14))
                    .foregroundStyle(Design.Colors.foreground)
                    .frame(width: 20, alignment: .center)

                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    if let subtitle {
                        Text(subtitle)
                            .font(Design.Typography.caption2)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }

                Spacer(minLength: Design.Spacing.sm)

                ZStack(alignment: isOn.wrappedValue ? .trailing : .leading) {
                    Capsule()
                        .fill(
                            isOn.wrappedValue
                                ? Design.Brand.accent
                                : Design.Colors.border
                        )
                        .frame(width: 46, height: 28)
                    Circle()
                        .fill(.white)
                        .frame(width: 24, height: 24)
                        .padding(2)
                }
                .animation(Design.Motion.quickResponse, value: isOn.wrappedValue)
                .accessibilityHidden(true)
            }
            .frame(minHeight: Design.Size.minTapTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isOn.wrappedValue ? [.isSelected] : [])
        .accessibilityValue(isOn.wrappedValue ? "on" : "off")
    }
}
