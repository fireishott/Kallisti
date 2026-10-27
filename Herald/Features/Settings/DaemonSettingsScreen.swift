import SwiftUI

// MARK: - The daemon's settings table, on the phone
//
// Everything on this screen comes from `GET /v1/settings`'s `capabilities`
// record. The daemon publishes each setting's label, its tab, its type, the value
// it currently reads, and the closed list it accepts, in the same tab order its
// own web UI renders. So this is not a second settings project: it is the
// daemon's table, drawn.
//
// That matters for the reason settings screens normally rot. A field added to the
// daemon appears here with no client change; a field whose options change gets the
// new list; a field that stops being writable stops offering a control. There is
// one table and two readers, which is what the daemon's own comment says it
// wanted, and it is why this file has no list of field names in it.
//
// The route needs the `admin` scope. A device paired from a code minted without
// the administration box gets the daemon's own sentence back, and that sentence is
// what this screen shows - verbatim, because it names the exact fix.

struct DaemonSettingsScreen: View {
    @Environment(ChatStore.self) private var chatStore

    /// The daemon's tabs, in its order.
    @State private var groups: [ErisSettingsGroup] = []
    @State private var isLoading = false
    /// What the daemon refused, in its own words.
    @State private var errorMessage: String?
    /// A short confirmation for the last write.
    @State private var notice: String?
    /// The field being written, so its control can show that it is in flight.
    @State private var writingKey: String?
    /// Text being typed into a free-text field, before it is committed.
    @State private var drafts: [String: String] = [:]

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            ScrollView(.vertical) {
                VStack(spacing: Design.Spacing.lg) {
                    if let errorMessage {
                        errorCard(errorMessage)
                    }
                    if let notice {
                        Text(notice)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    if groups.isEmpty, !isLoading, errorMessage == nil {
                        Text("The daemon reported no settings.")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    ForEach(groups) { group in
                        SettingsSectionView(title: group.title) {
                            VStack(spacing: 0) {
                                ForEach(Array(group.fields.enumerated()), id: \.element.id) { index, field in
                                    if index > 0 { divider }
                                    row(for: field)
                                }
                            }
                        }
                    }
                    if isLoading, groups.isEmpty {
                        ProgressView()
                            .tint(Design.Colors.accent)
                            .padding(.top, Design.Spacing.lg)
                    }
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.sm)
            }
            .refreshable { await load() }
        }
        .navigationTitle("Daemon")
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
    }

    // MARK: - Rows

    @ViewBuilder
    private func row(for field: ErisSettingField) -> some View {
        switch field.kind {
        case .bool:
            toggleRow(field)
        case .enumeration:
            choiceRow(field)
        case .int:
            numberRow(field)
        case .string:
            textRow(field)
        }
    }

    private func toggleRow(_ field: ErisSettingField) -> some View {
        Toggle(isOn: Binding(
            get: { field.boolValue },
            set: { newValue in
                Task { await write(.flag(field.key, newValue), label: field.label, on: newValue ? "on" : "off") }
            }
        )) {
            label(for: field)
        }
        .tint(Design.Colors.accent)
        .disabled(!field.writable || writingKey == field.key)
        .frame(minHeight: Design.Size.minTapTarget)
    }

    private func choiceRow(_ field: ErisSettingField) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            label(for: field)
            Spacer()
            // A menu rather than a picker wheel: the lists are short and the row
            // has to keep its height so a long tab does not jump while scrolling.
            Menu {
                ForEach(field.options, id: \.self) { option in
                    Button {
                        // The daemon's own list is the only source of a valid
                        // value, so a choice it does not offer is not a choice.
                        guard let pending = ErisSettingWrite.choice(field, option) else { return }
                        Task { await write(pending, label: field.label, on: pending.value) }
                    } label: {
                        if option == field.selectedOption {
                            Label(field.label(for: option), systemImage: "checkmark")
                        } else {
                            Text(field.label(for: option))
                        }
                    }
                }
            } label: {
                HStack(spacing: Design.Spacing.xxs) {
                    Text(field.selectedOption.map { field.label(for: $0) } ?? field.value)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            }
            .disabled(!field.writable || writingKey == field.key)
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    private func numberRow(_ field: ErisSettingField) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            label(for: field)
            Spacer()
            Text("\(field.intValue)")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .monospacedDigit()
            Stepper(
                "",
                value: Binding(
                    get: { field.intValue },
                    set: { newValue in
                        Task {
                            await write(
                                .number(field.key, newValue, min: field.min, max: field.max),
                                label: field.label,
                                on: "\(newValue)"
                            )
                        }
                    }
                ),
                in: (field.min ?? 0)...(field.max ?? max(field.intValue, 1))
            )
            .labelsHidden()
            .disabled(!field.writable || writingKey == field.key)
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    private func textRow(_ field: ErisSettingField) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            label(for: field)
            TextField(
                field.value.isEmpty ? "Not set" : field.value,
                text: Binding(
                    get: { drafts[field.key] ?? "" },
                    set: { drafts[field.key] = $0 }
                )
            )
            .font(Design.Typography.callout)
            .foregroundStyle(Design.Colors.foreground)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .submitLabel(.done)
            .disabled(!field.writable || writingKey == field.key)
            .onSubmit {
                guard let typed = drafts[field.key] else { return }
                Task {
                    // A cleared field is not a setting: the daemon treats an empty
                    // value as "no value", which is not what clearing meant, so
                    // the write is refused here rather than stored as blank.
                    guard let pending = ErisSettingWrite.text(field.key, typed) else {
                        notice = "\(field.label) cannot be empty."
                        return
                    }
                    // The field keeps showing the old value until the daemon has
                    // the new one, so a refused write does not look applied.
                    drafts[field.key] = nil
                    await write(pending, label: field.label, on: pending.value)
                }
            }
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    private func label(for field: ErisSettingField) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(field.label)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
            if !field.writable {
                Text("Read only on this daemon")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }
        }
    }

    private func errorCard(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            Text("The daemon refused")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
            Text(message)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
    }

    private var divider: some View {
        Rectangle()
            .fill(Design.Colors.border)
            .frame(height: 1)
            .padding(.vertical, Design.Spacing.xs)
    }

    // MARK: - Wire

    /// The transport for the paired daemon, when this build is on Eris.
    private func erisClient() async -> ErisClient? {
        guard let eris = chatStore.heraldClient as? ErisHeraldClient else { return nil }
        return await eris.client()
    }

    private func load() async {
        isLoading = true
        defer { isLoading = false }
        guard let client = await erisClient() else {
            errorMessage = "No Eris daemon is paired."
            return
        }
        do {
            let snapshot = try await client.settings()
            groups = snapshot.groups
            errorMessage = nil
        } catch {
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    private func write(_ change: ErisSettingWrite, label: String, on described: String) async {
        guard writingKey == nil else { return }
        guard let client = await erisClient() else { return }
        writingKey = change.key
        notice = nil
        defer { writingKey = nil }
        do {
            let changed = try await client.updateSetting(change)
            if changed.isEmpty {
                notice = "\(label) already read that."
            } else {
                notice = "\(label): \(described)"
            }
            await load()
        } catch {
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }
}
