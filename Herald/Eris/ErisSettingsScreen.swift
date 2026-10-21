import Observation
import SwiftUI

// MARK: - Settings view model

/// The daemon's operator-facing settings, as something a person can change from
/// a phone.
///
/// The screen reads, then edits a draft, then writes only the fields that
/// actually differ. Both routes need the `admin` scope, so the interesting state
/// here is a 403: a device paired from a code minted without the administration
/// box can be perfectly healthy and still have no settings at all. That refusal
/// carries the daemon's own sentence about the pairing code, and the screen shows
/// it rather than a generic "forbidden".
@MainActor
@Observable
final class ErisSettingsModel {
    /// What the daemon last reported.
    private(set) var snapshot: ErisSettingsSnapshot?
    /// Whether the first read is in flight.
    private(set) var isLoading = false
    /// Whether a write is in flight.
    private(set) var isSaving = false
    /// The refusal that stopped the read or the write.
    private(set) var failure: ErisFailureKind?
    /// The fields the last successful write actually changed.
    private(set) var savedFields: [String] = []
    /// Whether the snapshot has ever loaded.
    private(set) var hasLoaded = false

    /// The model draft, trimmed before it is compared or sent.
    var modelText = ""
    /// The sandbox draft.
    var sandbox = ""
    /// The memory backend draft.
    var memory = ""
    /// The approval policy draft.
    var approval = ""

    /// The transport. An actor, so nothing here blocks the main thread.
    private let client: ErisClient

    init(client: ErisClient) {
        self.client = client
    }

    // MARK: Derived state

    /// The subset that differs from what the daemon holds.
    ///
    /// `nil` before the first successful read, because there is nothing to
    /// compare against yet and sending a guess would write a value nobody chose.
    var patch: ErisSettingsPatch? {
        guard let snapshot else { return nil }
        return ErisSettingsPatch.changes(
            from: snapshot,
            model: modelText,
            sandbox: sandbox,
            memory: memory,
            approval: approval
        )
    }

    /// Whether the apply control has anything to do.
    var hasChanges: Bool {
        guard let patch else { return false }
        return !patch.isEmpty && !isModelEmpty
    }

    /// Whether the model draft is empty. The daemon would refuse it as
    /// `model_invalid`, so the screen refuses first and says why.
    var isModelEmpty: Bool {
        modelText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// The one-line report of the last write.
    var savedMessage: String? {
        guard !savedFields.isEmpty else { return nil }
        return "Changed: " + savedFields.joined(separator: ", ") + "."
    }

    // MARK: Reading

    /// Read the settings unless they are already on screen.
    func loadIfNeeded() async {
        guard !hasLoaded else { return }
        await load()
    }

    /// `GET /v1/settings`, and put the answer in the draft.
    func load() async {
        guard !isLoading else { return }
        isLoading = true
        failure = nil
        defer { isLoading = false }
        do {
            let fresh = try await client.settings()
            apply(fresh)
            hasLoaded = true
        } catch {
            failure = ErisFailureKind(error)
        }
    }

    // MARK: Writing

    /// `POST /v1/settings` with only the fields that changed.
    ///
    /// The daemon answers which fields it wrote, so that list is what the screen
    /// reports rather than the list this app thought it sent.
    func save() async {
        guard let patch, !patch.isEmpty, !isSaving else { return }
        if isModelEmpty {
            failure = .other(message: "a model id is required to change the model", code: nil)
            return
        }
        isSaving = true
        failure = nil
        savedFields = []
        defer { isSaving = false }
        do {
            let changed = try await client.updateSettings(patch)
            savedFields = changed
            // Re-read rather than trusting the draft: the daemon is the one place
            // that knows what a value normalizes to.
            let fresh = try await client.settings()
            apply(fresh)
            hasLoaded = true
        } catch {
            failure = ErisFailureKind(error)
        }
    }

    /// Throw the draft away and go back to what the daemon holds.
    func revert() {
        guard let snapshot else { return }
        apply(snapshot)
        failure = nil
        savedFields = []
    }

    /// Point the draft at one snapshot.
    private func apply(_ fresh: ErisSettingsSnapshot) {
        snapshot = fresh
        modelText = fresh.model
        sandbox = fresh.sandbox
        memory = fresh.memory
        approval = fresh.approval
    }
}

// MARK: - Settings screen

/// The settings surface: seven values, four of them editable, and an honest
/// account of why the other three are read-only and why the whole screen can be
/// closed to this device.
struct ErisSettingsScreen: View {
    let store: ErisDaemonStore
    let model: ErisSettingsModel

    @State private var isPermissionsPresented = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Design.Spacing.lg) {
                content
            }
            .padding(Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
        }
        .background(Design.Colors.background)
        .task { await model.loadIfNeeded() }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button {
                    isPermissionsPresented = true
                } label: {
                    Label("Permissions", systemImage: "hand.raised")
                        .font(Design.Typography.footnote)
                }
                .accessibilityIdentifier("eris.permissions")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await model.load() }
                } label: {
                    Label("Reload", systemImage: "arrow.clockwise")
                        .font(Design.Typography.footnote)
                }
                .disabled(model.isLoading || model.isSaving)
                .accessibilityIdentifier("eris.reloadSettings")
            }
        }
        .sheet(isPresented: $isPermissionsPresented) {
            NavigationStack {
                PermissionsScreen()
            }
        }
    }

    // MARK: States

    @ViewBuilder
    private var content: some View {
        if model.isLoading, !model.hasLoaded {
            ErisLoadingView(label: "Reading the daemon's settings…")
        } else if let failure = model.failure, !model.hasLoaded {
            failureBlock(failure)
        } else if let snapshot = model.snapshot {
            editable(snapshot)
            readOnly(snapshot)
            applyControls
            if let failure = model.failure {
                failureBlock(failure)
            }
        } else {
            failureBlock(.other(message: "the daemon returned no settings", code: nil))
        }
    }

    /// The daemon's own refusal, plus the explanation of what to do about it.
    ///
    /// For a scope refusal the headline and the body come straight from
    /// `require_admin`: the pairing code was minted without the administration
    /// box, and the fix is to mint one with it ticked.
    private func failureBlock(_ failure: ErisFailureKind) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            ErisFailureView(
                kind: failure,
                onRetry: { Task { await model.load() } },
                onPairAgain: { Task { await store.forget() } }
            )
            if case .adminScopeDenied = failure {
                Text("Settings are the only thing behind the admin scope, so the rest of Eris works normally on this device.")
                    .font(Design.Typography.helper)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("eris.settingsScopeNote")
            }
        }
    }

    // MARK: Editable

    @ViewBuilder
    private func editable(_ snapshot: ErisSettingsSnapshot) -> some View {
        section("Model") {
            TextField("model id", text: Bindable(model).modelText)
                .font(Design.Typography.code)
                .foregroundStyle(Design.Colors.foreground)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .padding(Design.Spacing.sm)
                .background(Design.Colors.surface2)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
                .accessibilityIdentifier("eris.settingsModel")
            if model.isModelEmpty {
                Text("A model id is required. The daemon refuses an empty one as model_invalid.")
                    .font(Design.Typography.helper)
                    .foregroundStyle(Design.Colors.warning)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                Text("Stored: \(snapshot.displayModel)")
                    .font(Design.Typography.caption2)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }
        }

        section("Sandbox") {
            Picker("Sandbox", selection: Bindable(model).sandbox) {
                ForEach(ErisSettingsOption.sandbox, id: \.self) { value in
                    Text(ErisSettingsOption.sandboxLabel(value)).tag(value)
                }
            }
            .pickerStyle(.menu)
            .tint(Design.Colors.accent)
            .accessibilityIdentifier("eris.settingsSandbox")
            Text(ErisSettingsOption.sandboxConsequence(model.sandbox))
                .font(Design.Typography.helper)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .fixedSize(horizontal: false, vertical: true)
        }

        section("Memory") {
            Picker("Memory", selection: Bindable(model).memory) {
                ForEach(ErisSettingsOption.memory, id: \.self) { value in
                    Text(ErisSettingsOption.memoryLabel(value)).tag(value)
                }
            }
            .pickerStyle(.menu)
            .tint(Design.Colors.accent)
            .accessibilityIdentifier("eris.settingsMemory")
            Text("Where a turn looks for what it already knows: the brain, the vault, or nowhere.")
                .font(Design.Typography.helper)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .fixedSize(horizontal: false, vertical: true)
        }

        section("Approval") {
            Picker("Approval", selection: Bindable(model).approval) {
                ForEach(ErisSettingsOption.approval, id: \.self) { value in
                    Text(ErisSettingsOption.approvalLabel(value)).tag(value)
                }
            }
            .pickerStyle(.segmented)
            .accessibilityIdentifier("eris.settingsApproval")
            Text(ErisSettingsOption.approvalConsequence(model.approval))
                .font(Design.Typography.helper)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: Read only

    private func readOnly(_ snapshot: ErisSettingsSnapshot) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text("Reported, not editable here")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)
            readOnlyRow("Tools", value: snapshot.toolsEnabled ? "on" : "off")
            readOnlyRow("Tool rounds", value: "\(snapshot.toolRounds)")
            readOnlyRow("Timezone", value: snapshot.timezone.isEmpty ? "unset" : snapshot.timezone)
            Text("The settings route accepts model, sandbox, memory and approval only.")
                .font(Design.Typography.helper)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
    }

    private func readOnlyRow(_ label: String, value: String) -> some View {
        HStack {
            Text(label)
                .font(Design.Typography.body)
                .foregroundStyle(Design.Colors.secondaryForeground)
            Spacer()
            Text(value)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.foreground)
        }
    }

    // MARK: Apply

    private var applyControls: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Button {
                Task { await model.save() }
            } label: {
                Text(model.isSaving ? "Applying…" : "Apply changes")
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Colors.background)
                    .frame(maxWidth: .infinity, minHeight: Design.Size.minTapTarget)
                    .background(Design.Brand.accent)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
            }
            .buttonStyle(.plain)
            .disabled(!model.hasChanges || model.isSaving)
            .opacity(model.hasChanges && !model.isSaving ? 1 : 0.55)
            .accessibilityIdentifier("eris.applySettings")

            if let patch = model.patch, !patch.isEmpty {
                Text("Will send: " + patch.changedKeys.joined(separator: ", "))
                    .font(Design.Typography.caption2)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
                    .accessibilityIdentifier("eris.settingsPending")
            } else {
                Text("Nothing to apply: every field matches the daemon.")
                    .font(Design.Typography.caption2)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }

            if let saved = model.savedMessage {
                Text(saved)
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.success)
                    .accessibilityIdentifier("eris.settingsSaved")
            }

            if model.hasChanges {
                Button("Discard changes") { model.revert() }
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .frame(minHeight: Design.Size.minTapTarget)
                    .accessibilityIdentifier("eris.revertSettings")
            }
        }
    }

    // MARK: Layout

    private func section<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text(title)
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)
            content()
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
    }
}
