import SwiftUI

/// One setting on a group screen: label, hint, control.
///
/// The control is chosen from the record's own `type`, never from a switch on the
/// key, so a new field takes the control its kind implies with no client change.
/// An unknown kind renders the raw value read-only instead of nothing.
struct DaemonFieldRow: View {
    let setting: DaemonSetting
    let refusal: String?
    @Binding var draft: String
    let onBool: (Bool) -> Void
    let onDirty: () -> Void

    init(
        setting: DaemonSetting,
        refusal: String?,
        draft: Binding<String>,
        onBool: @escaping (Bool) -> Void,
        onDirty: @escaping () -> Void
    ) {
        self.setting = setting
        self.refusal = refusal
        self._draft = draft
        self.onBool = onBool
        self.onDirty = onDirty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(alignment: .firstTextBaseline, spacing: Design.Spacing.sm) {
                Text(setting.label)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
                    .frame(maxWidth: .infinity, alignment: .leading)
                control
            }
            .frame(minHeight: 44)

            if let refusal {
                Text(refusal)
                    .font(Design.Typography.helper)
                    .foregroundStyle(Design.Colors.danger)
                    .fixedSize(horizontal: false, vertical: true)
            } else if !setting.writable {
                Text("Read-only here")
                    .font(Design.Typography.helper)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }
        }
        .padding(.vertical, Design.Spacing.xs)
    }

    @ViewBuilder
    private var control: some View {
        switch setting.kind {
        case .bool:
            Toggle("", isOn: Binding(
                get: { (draft.isEmpty ? setting.value : draft) == "true" },
                set: { onBool($0) }
            ))
            .labelsHidden()
            .tint(Design.Brand.accent)

        case .enumeration:
            Picker(setting.label, selection: Binding(
                get: { draft.isEmpty ? setting.value : draft },
                set: { draft = $0; onDirty() }
            )) {
                ForEach(setting.options ?? [], id: \.self) { option in
                    Text(DaemonSetting.optionLabel(option)).tag(option)
                }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .tint(Design.Brand.accent)

        case .int:
            HStack(spacing: Design.Spacing.sm) {
                Text(numberValue)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
                if let bounds = setting.intBounds {
                    Stepper("", value: Binding(
                        get: { Int(draft.isEmpty ? setting.value : draft) ?? bounds.lowerBound },
                        set: { next in
                            draft = String(min(max(next, bounds.lowerBound), bounds.upperBound))
                            onDirty()
                        }
                    ), in: bounds)
                    .labelsHidden()
                }
            }

        case .string:
            TextField("", text: $draft)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .multilineTextAlignment(.trailing)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.foreground)
                .onChange(of: draft) { _, _ in onDirty() }

        case .unknown:
            Text(setting.value.isEmpty ? "-" : setting.value)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .multilineTextAlignment(.trailing)
        }
    }

    private var numberValue: String {
        draft.isEmpty ? setting.value : draft
    }
}
