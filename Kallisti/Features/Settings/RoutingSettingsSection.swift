import Foundation
import SwiftUI

/// What the daemon serves at `/v1/routing`, read for the parts the Routing card
/// draws. `ok` and `provider_model` travel too and are deliberately not decoded:
/// nothing here renders them.
/// Which of the three catalogue model rows a pick applies to. The harness model
/// is not one of them: its choices come from the selected harness, not from the
/// daemon's catalogue.
enum ModelField: String, Identifiable {
    case coding, heavy, general

    var id: String { rawValue }

    var title: String {
        switch self {
        case .coding: "Coding model"
        case .heavy: "Heavy model"
        case .general: "General model"
        }
    }
}

private struct RoutingPayload: Decodable {
    let auto: Bool
    let codingModel: String?
    let heavyModel: String?
    let generalModel: String?
    let harness: String?
    let harnesses: [String]?
    let harnessModel: String?
    let models: [RoutingModel]?

    enum CodingKeys: String, CodingKey {
        case auto
        case codingModel = "coding_model"
        case heavyModel = "heavy_model"
        case generalModel = "general_model"
        case harness
        case harnesses
        case harnessModel = "harness_model"
        case models
    }
}

/// One model a harness says it supports. `label` is what the picker prints and
/// `id` is what `PUT /v1/routing` stores, so the two never have to be the same
/// string.
struct HarnessToolModel: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let label: String

    private enum CodingKeys: String, CodingKey { case id, label }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = (try? container.decode(String.self, forKey: .label)) ?? id
    }
}

/// What a harness reports about its sign-in.
///
/// `source` is decoded because the contract carries it, but nothing here draws
/// it: where the credential was found is the daemon's business. The token is
/// never named at all.
struct HarnessToolAuth: Decodable, Sendable {
    let kind: String
    let verified: Bool
    let account: String?
    let source: String?

    private enum CodingKeys: String, CodingKey { case kind, verified, account, source }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = (try? container.decode(String.self, forKey: .kind)) ?? ""
        verified = (try? container.decode(Bool.self, forKey: .verified)) ?? false
        account = try? container.decodeIfPresent(String.self, forKey: .account)
        source = try? container.decodeIfPresent(String.self, forKey: .source)
    }
}

/// One row of `GET /v1/harness/tools`.
///
/// Every field the contract names is decoded; only `name`, `version`,
/// `installed`, `installable`, `install_hint`, `auth` and `models` are drawn.
/// `path` is deliberately not rendered: a filesystem path is daemon detail, not
/// something the person choosing a harness needs.
struct HarnessTool: Decodable, Identifiable, Sendable {
    let id: String
    let name: String
    let binary: String
    let installed: Bool
    let version: String?
    let path: String?
    let installable: Bool
    let installHint: String
    let auth: HarnessToolAuth?
    let models: [HarnessToolModel]

    private enum CodingKeys: String, CodingKey {
        case id
        case name
        case binary
        case installed
        case version
        case path
        case installable
        case auth
        case models
        case installHint = "install_hint"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        name = (try? container.decode(String.self, forKey: .name)) ?? id
        binary = (try? container.decode(String.self, forKey: .binary)) ?? ""
        installed = (try? container.decode(Bool.self, forKey: .installed)) ?? false
        version = try? container.decodeIfPresent(String.self, forKey: .version)
        path = try? container.decodeIfPresent(String.self, forKey: .path)
        installable = (try? container.decode(Bool.self, forKey: .installable)) ?? false
        installHint = (try? container.decode(String.self, forKey: .installHint)) ?? ""
        auth = try? container.decodeIfPresent(HarnessToolAuth.self, forKey: .auth)
        models = (try? container.decode([HarnessToolModel].self, forKey: .models)) ?? []
    }
}

private struct HarnessToolsPayload: Decodable {
    let tools: [HarnessTool]
}

/// What one routing PUT came back with. The body is kept for a refusal because
/// that is the reply the daemon explains itself in.
private enum RoutingPutOutcome {
    case stored(RoutingPayload?)
    case refused(status: Int, body: Data)
    case failed(reason: String)
    case offline
}

/// Routing: whether the daemon picks the model for each turn, and which model
/// and harness each kind of turn goes to.
///
/// Self-contained on purpose. It reads and writes `/v1/routing` itself, so a
/// Settings screen that never reaches this card makes no request, and it takes
/// nothing but the environment: the daemon's base URL and the device's access
/// token are acquired exactly the way the speech rows on `SettingsScreen`
/// acquire them.
struct RoutingSettingsSection: View {
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(AppSessionStore.self) private var sessionStore

    /// The daemon's `auto` flag.
    @State private var isAutomatic = false
    @State private var codingModel = ""
    @State private var heavyModel = ""
    @State private var generalModel = ""
    @State private var harnessModel = ""
    /// The daemon's configured model ids. The fields are pickers over this, so a
    /// value that no provider holds cannot be typed in and then silently fail.
    @State private var routingModels: [RoutingModel] = []
    /// Which field's sheet is open, if any. One sheet serves all four.
    @State private var pickingModelFor: ModelField?
    /// The Claude sign-in sheet: a token pasted from the host, or the command to
    /// produce one there.
    @State private var showingClaudeSignIn = false
    /// Build 242: the token field is open on arrival. Collapsed, the popup read
    /// as if a pasted token had been removed.
    @State private var showTokenInstead = true
    @State private var claudeTokenInput = ""
    @State private var claudeSignInError: String?
    @State private var isSavingClaudeToken = false
    @State private var claudeLoginID = ""
    @State private var claudeLoginCode = ""
    @State private var isStartingClaudeLogin = false
    @Environment(\.openURL) private var openURL
    /// `none`, or one of the daemon's harness ids.
    @State private var harness = "none"
    /// The harness ids the daemon offers, in its own order.
    @State private var harnesses: [String] = []
    /// What `GET /v1/harness/tools` reports, one row per harness. Empty means
    /// the daemon does not serve the read (404) or could not be asked, and the
    /// rows stay hidden rather than inventing a state the daemon did not send.
    @State private var harnessTools: [HarnessTool] = []
    /// The tool whose install POST is in flight, if any.
    @State private var installingToolID: String?
    /// A Verify tap is re-reading the tools.
    @State private var isVerifyingTools = false
    @State private var routingError: String?
    @State private var hasLoaded = false
    @State private var isSaving = false
    @State private var isStoringHarness = false
    /// The daemon's harness runs behind the monitor row. This card owns its own
    /// instance of the shared model: Settings and the chat banner are never on
    /// screen together, so one poller per presentation site is enough.
    @State private var harnessMonitor = HarnessMonitorModel()
    @State private var showHarnessMonitor = false

    var body: some View {
        SettingsSectionView(title: "Routing") {
            VStack(spacing: 0) {
                automaticRow

                sectionDivider

                modelFields

                sectionDivider

                harnessRow

                if !harnessTools.isEmpty {
                    sectionDivider

                    harnessToolsSection
                }

                sectionDivider

                caption

                sectionDivider

                harnessMonitorRow

                if let routingError {
                    Text(routingError)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.warning)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.top, Design.Spacing.xxs)
                        .accessibilityIdentifier("settings.routing.error")
                }
            }
        }
        .task { await loadRouting() }
        .task { await loadHarnessTools() }
        .task {
            // One read on appearance is enough for the count here; the monitor
            // sheet it opens owns the two-second poll while it is on screen.
            await harnessMonitor.refreshList(daemon: harnessDaemon())
        }
        .sheet(item: $pickingModelFor) { field in
            ModelPickerSheet(
                title: field.title,
                models: routingModels,
                selection: binding(for: field).wrappedValue
            ) { id in
                binding(for: field).wrappedValue = id
                // A pick is the end of the question: close the sheet instead of
                // making the person dismiss a window they are done with.
                pickingModelFor = nil
            } onDismiss: {
                pickingModelFor = nil
            }
        }
        .sheet(isPresented: $showingClaudeSignIn) {
            claudeSignInSheet
        }
        .sheet(isPresented: $showHarnessMonitor) {
            HarnessMonitorSheet(onDismiss: { showHarnessMonitor = false })
                .environment(harnessMonitor)
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
        }
    }

    // MARK: - Rows

    /// The same full-row switch the rest of Settings draws: the whole row is
    /// the tap target, not just the switch.
    private var automaticRow: some View {
        Button {
            automaticBinding.wrappedValue.toggle()
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 14))
                    .foregroundStyle(Design.Brand.accent)
                    .frame(width: 20, alignment: .center)

                VStack(alignment: .leading, spacing: 1) {
                    Text("Automatic routing")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    Text("The daemon picks the model for each turn.")
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }

                Spacer(minLength: Design.Spacing.sm)

                ZStack(alignment: isAutomatic ? .trailing : .leading) {
                    Capsule()
                        .fill(isAutomatic ? Design.Brand.accent : Design.Colors.border)
                        .frame(width: 46, height: 28)
                    Circle()
                        .fill(.white)
                        .frame(width: 24, height: 24)
                        .padding(2)
                }
                .animation(Design.Motion.quickResponse, value: isAutomatic)
            }
            .frame(minHeight: Design.Size.minTapTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!hasLoaded || isSaving)
        .accessibilityIdentifier("settings.routing.auto")
    }

    /// The three models, and the one button that stores them.
    ///
    /// All three travel on every save, together with `auto`: the daemon takes a
    /// subset, so a save that named only the field the person touched would
    /// leave the other two untouched and the row would lie about what it stored.
    private var modelFields: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            modelField("Coding model", field: .coding, text: $codingModel)
            modelField("Heavy model", field: .heavy, text: $heavyModel)
            modelField("General model", field: .general, text: $generalModel)
            if harness != "none" {
                harnessModelField
            }

            Button {
                Task { await saveModels() }
            } label: {
                HStack(spacing: Design.Spacing.sm) {
                    if isSaving {
                        ProgressView()
                            .controlSize(.small)
                            .tint(Design.Brand.accent)
                    } else {
                        Image(systemName: "checkmark.circle.fill")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.accent)
                            .frame(width: 20, alignment: .center)
                    }

                    Text(isSaving ? "Saving…" : "Save")
                        .font(Design.Typography.callout)
                        .foregroundStyle(
                            isSaving
                                ? Design.Colors.secondaryForeground
                                : Design.Colors.foreground
                        )

                    Spacer()
                }
                .frame(minHeight: Design.Size.minTapTarget)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!hasLoaded || isSaving)
            .accessibilityIdentifier("settings.routing.save")

            Text("An empty field inherits the daemon's own model for that kind of turn.")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.vertical, Design.Spacing.xs)
    }

    private func modelField(_ label: String, field: ModelField, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
            Text(label)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)

            // A row that opens a searchable, provider-grouped sheet. A menu of
            // several hundred ids named the model and hid where it runs.
            Button {
                pickingModelFor = field
            } label: {
                HStack(spacing: Design.Spacing.sm) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(text.wrappedValue.isEmpty ? "The daemon's own" : text.wrappedValue)
                            .font(Design.Typography.callout)
                            .foregroundStyle(
                                text.wrappedValue.isEmpty
                                    ? Design.Colors.secondaryForeground
                                    : Design.Colors.foreground
                            )
                            .lineLimit(1)
                        if let provider = providerLabel(for: text.wrappedValue) {
                            Text(provider)
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    }
                    Spacer()
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .padding(Design.Spacing.md)
                .background(Design.Colors.background)
                .overlay(
                    RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                        .stroke(Design.Colors.border, lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
            }
            .buttonStyle(.plain)
            .disabled(!hasLoaded || isSaving || routingModels.isEmpty)
        }
    }

    /// The harness model is not a daemon-catalogue pick.
    ///
    /// Each harness supports its own set, so the menu is built from the selected
    /// tool's `models` and the empty tag means "let the tool decide". A value the
    /// rows do not list is kept as a tag of its own so the stored value is not
    /// silently hidden before `Save` gets a chance to clear it.
    private var harnessModelField: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
            Text("Harness model")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)

            Picker("Harness model", selection: $harnessModel) {
                Text("The tool's own default").tag("")
                ForEach(selectedHarnessModels) { model in
                    Text(model.label.isEmpty ? model.id : model.label).tag(model.id)
                }
                if !harnessModel.isEmpty,
                   !selectedHarnessModels.contains(where: { $0.id == harnessModel }) {
                    Text(harnessModel).tag(harnessModel)
                }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .tint(Design.Colors.foreground)
            .padding(Design.Spacing.md)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Design.Colors.background)
            .overlay(
                RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                    .stroke(Design.Colors.border, lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
            .disabled(!hasLoaded || isSaving)
            .accessibilityIdentifier("settings.routing.harnessModel")
        }
    }

    /// The tool the harness picker names, when the tools read has answered.
    private var selectedHarnessTool: HarnessTool? {
        harnessTools.first { $0.id == harness }
    }

    private var selectedHarnessModels: [HarnessToolModel] {
        selectedHarnessTool?.models ?? []
    }

    /// Signing Claude Code in from the phone.
    ///
    /// The tool wants a browser, and the daemon is a service with no display, so
    /// the honest path is: run the command that prints a token on the host, paste
    /// it here, and the daemon stores it as the credential it checks first.
    private var claudeSignInSheet: some View {
        NavigationStack {
            Form {
                Section {
                    if claudeLoginID.isEmpty {
                        Text("Sign in with the account Claude Code runs as. The daemon opens the same sign-in the tool uses, and takes the code back.")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    } else {
                        Text("Finish in the browser that just opened, then paste the code it shows.")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }

                if claudeLoginID.isEmpty {
                    Section {
                        Button {
                            Task { await startClaudeLogin() }
                        } label: {
                            HStack {
                                if isStartingClaudeLogin {
                                    ProgressView().controlSize(.small)
                                }
                                Text(isStartingClaudeLogin ? "Opening…" : "Sign in with Claude")
                            }
                        }
                        .disabled(isStartingClaudeLogin)
                        .accessibilityIdentifier("settings.routing.claude.signin.start")
                    }
                } else {
                    Section("Code") {
                        TextField("Paste the code", text: $claudeLoginCode)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.system(.callout, design: .monospaced))
                        Button {
                            Task { await finishClaudeLogin() }
                        } label: {
                            HStack {
                                if isSavingClaudeToken {
                                    ProgressView().controlSize(.small)
                                }
                                Text(isSavingClaudeToken ? "Checking…" : "Finish sign-in")
                            }
                        }
                        .disabled(claudeLoginCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isSavingClaudeToken)
                        .accessibilityIdentifier("settings.routing.claude.signin.finish")
                    }
                }

                if let claudeSignInError {
                    Section {
                        Text(claudeSignInError)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.warning)
                    }
                }

                Section {
                    DisclosureGroup("Use a token instead", isExpanded: $showTokenInstead) {
                        SecureField("sk-ant-oat…", text: $claudeTokenInput)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.system(.callout, design: .monospaced))
                        Button("Save token") {
                            Task { await saveClaudeToken() }
                        }
                        .disabled(claudeTokenInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isSavingClaudeToken)
                    }
                    .font(Design.Typography.caption)
                }
            }
            .navigationTitle("Claude Code sign-in")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") {
                        showingClaudeSignIn = false
                        claudeLoginID = ""
                        claudeLoginCode = ""
                    }
                }
            }
        }
    }

    private func startClaudeLogin() async {
        guard !isStartingClaudeLogin else { return }
        isStartingClaudeLogin = true
        defer { isStartingClaudeLogin = false }
        claudeSignInError = nil
        guard let url = daemonURL("/v1/harness/tools/claude-code/login/start") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            if status == 403 {
                claudeSignInError = "This phone was paired without administration, so the daemon refuses the sign-in."
                return
            }
            let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            guard (200..<300).contains(status),
                  let id = body?["id"] as? String,
                  let link = body?["url"] as? String,
                  let linkURL = URL(string: link)
            else {
                let code = (body?["code"] as? String) ?? "unknown"
                let message = (body?["message"] as? String) ?? ""
                claudeSignInError = "The daemon refused (\(code), \(status)). \(message)"
                return
            }
            claudeLoginID = id
            openURL(linkURL)
        } catch {
            claudeSignInError = "The sign-in could not open: \(error.localizedDescription)"
        }
    }

    private func finishClaudeLogin() async {
        guard !isSavingClaudeToken else { return }
        isSavingClaudeToken = true
        defer { isSavingClaudeToken = false }
        claudeSignInError = nil
        // A silent return here left the sheet spinning with no message.
        //
        // If the base URL went missing, or the sheet was reopened without a session
        // id, this returned early and the operator saw nothing happen at all - the
        // worst kind of failure, because there is nothing to report and nothing to
        // retry. Both conditions now say which one it was.
        guard !claudeLoginID.isEmpty else {
            claudeSignInError = "This sign-in has no session. Close this and tap Sign in with Claude again."
            return
        }
        guard let url = daemonURL("/v1/harness/tools/claude-code/login/complete") else {
            claudeSignInError = "No daemon address is configured, so the code had nowhere to go."
            return
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        request.httpBody = try? JSONSerialization.data(withJSONObject: [
            "id": claudeLoginID,
            "code": claudeLoginCode.trimmingCharacters(in: .whitespacesAndNewlines),
        ])
        // Outlast the daemon's own wait, or this fails while the daemon is still
        // working.
        //
        // The daemon holds this request open while `claude setup-token` exchanges
        // the code, and that wait was raised from 30 seconds to 120 because a real
        // exchange can outlast a shorter one. `URLSession`'s default is 60, so the
        // app hung up first and reported "the code could not be sent" - an error
        // about the network for a request the daemon was still answering. The
        // ceiling here must be above the daemon's, never below it.
        request.timeoutInterval = 150
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            guard (200..<300).contains(status) else {
                let code = (body?["code"] as? String) ?? "unknown"
                let message = (body?["message"] as? String) ?? ""
                claudeSignInError = "The daemon refused (\(code), \(status)). \(message)"
                return
            }
            claudeLoginCode = ""
            claudeLoginID = ""
            showingClaudeSignIn = false
            await loadHarnessTools()
        } catch {
            claudeSignInError = "The code could not be sent: \(error.localizedDescription)"
        }
    }

    private func saveClaudeToken() async {
        guard !isSavingClaudeToken else { return }
        isSavingClaudeToken = true
        defer { isSavingClaudeToken = false }
        claudeSignInError = nil
        guard let url = daemonURL("/v1/harness/tools/claude-code/auth") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "PUT"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        request.httpBody = try? JSONSerialization.data(withJSONObject: [
            "token": claudeTokenInput.trimmingCharacters(in: .whitespacesAndNewlines)
        ])
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            if status == 403 {
                claudeSignInError = "This phone was paired without administration, so the daemon refuses the token."
                return
            }
            guard (200..<300).contains(status) else {
                let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
                claudeSignInError = "The daemon refused (\((body?["code"] as? String) ?? "unknown"), \(status))."
                return
            }
            try? JSONSerialization.jsonObject(with: data)
            claudeTokenInput = ""
            showingClaudeSignIn = false
            await loadHarnessTools()
        } catch {
            claudeSignInError = "The token could not be sent: \(error.localizedDescription)"
        }
    }

    /// The provider that publishes a chosen id, when the catalogue still lists it.
    private func providerLabel(for id: String) -> String? {
        guard !id.isEmpty else { return nil }
        return routingModels.first { $0.id == id }?.providerLabel
    }

    private func binding(for field: ModelField) -> Binding<String> {
        switch field {
        case .coding: $codingModel
        case .heavy: $heavyModel
        case .general: $generalModel
        }
    }

    /// The harness a coding turn is handed to. The list is the daemon's; `none`
    /// is always offered so a harness can be cleared even by a daemon that
    /// leaves it out of its own list.
    private var harnessRow: some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: "hammer")
                .font(.system(size: 14))
                .foregroundStyle(Design.Brand.accent)
                .frame(width: 20, alignment: .center)

            Text("Coding harness")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)

            Spacer()

            Picker("Coding harness", selection: harnessBinding) {
                if !harnessOptions.contains(harness), !harness.isEmpty {
                    Text(harnessLabel(harness)).tag(harness)
                }
                ForEach(harnessOptions, id: \.self) { id in
                    Text(harnessLabel(id)).tag(id)
                }
            }
            .pickerStyle(.menu)
            .tint(Design.Colors.secondaryForeground)
            .disabled(!hasLoaded || isStoringHarness)
            .accessibilityIdentifier("settings.routing.harness")
        }
        .frame(minHeight: Design.Size.minTapTarget)
        .padding(.vertical, Design.Spacing.xs)
    }

    /// One row per harness the daemon reported: whether it is on this host and,
    /// for Claude Code, whether its sign-in verified. The section is drawn only
    /// when the tools read answered with rows.
    private var harnessToolsSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            ForEach(harnessTools) { tool in
                harnessToolRow(tool)
            }
        }
        .padding(.vertical, Design.Spacing.xs)
    }

    private func harnessToolRow(_ tool: HarnessTool) -> some View {
        HStack(alignment: .top, spacing: Design.Spacing.sm) {
            Image(systemName: "circle.fill")
                .font(.system(size: 8))
                .foregroundStyle(tool.installed ? Design.Colors.success : Design.Colors.warning)
                .padding(.top, 6)
                .accessibilityLabel(tool.installed ? "Installed" : "Missing")

            VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
                HStack(spacing: Design.Spacing.xs) {
                    Text(tool.name)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    if let version = tool.version, !version.isEmpty {
                        Text(version)
                            .font(Design.Typography.caption2)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                    }
                }

                if tool.id == "claude-code" {
                    Text(authLine(tool.auth))
                        .font(Design.Typography.caption)
                        .foregroundStyle(
                            tool.auth?.verified == true
                                ? Design.Colors.secondaryForeground
                                : Design.Colors.warning
                        )
                }

                if !tool.installed && !tool.installable && !tool.installHint.isEmpty {
                    // The daemon cannot install this one, so the row says how a
                    // person would. Monospaced because it is a command, not prose.
                    Text(tool.installHint)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            Spacer(minLength: Design.Spacing.sm)

            if !tool.installed && tool.installable {
                Button {
                    Task { await install(tool) }
                } label: {
                    if installingToolID == tool.id {
                        ProgressView()
                            .controlSize(.small)
                            .tint(Design.Brand.accent)
                    } else {
                        Text("Install")
                            .font(Design.Typography.caption)
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(Design.Brand.accent)
                .disabled(installingToolID != nil || isSaving)
                .accessibilityIdentifier("settings.routing.tools.\(tool.id).install")
            } else if tool.installed && tool.id == "claude-code" {
                // Sign in is ALWAYS offered, and reads "Sign in again" once there is
                // a credential.
                //
                // It used to appear only while `verified != true`, so a harness that
                // reported a sign-in had no way to replace it - and a token can be
                // dead while the status still says present (the API answers 401 for a
                // token the CLI reports as logged in). The operator was left with
                // only Verify, which can confirm a broken state but not fix it.
                //
                // The label carries the state instead of hiding the action: a
                // working sign-in gets "Sign in again", which is how you replace an
                // account, and a missing one gets the plain invitation.
                Button {
                    claudeSignInError = nil
                    showingClaudeSignIn = true
                } label: {
                    Text(tool.auth?.verified == true ? "Sign in again" : "Sign in")
                        .font(Design.Typography.caption)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .tint(Design.Brand.accent)
                .disabled(isSaving)
                .accessibilityIdentifier("settings.routing.tools.\(tool.id).signin")
                Button {
                    Task { await verifyHarnessTools() }
                } label: {
                    if isVerifyingTools {
                        ProgressView()
                            .controlSize(.small)
                            .tint(Design.Brand.accent)
                    } else {
                        Text("Verify")
                            .font(Design.Typography.caption)
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(Design.Brand.accent)
                .disabled(isVerifyingTools)
                .accessibilityIdentifier("settings.routing.tools.\(tool.id).verify")
            }
        }
        .frame(minHeight: Design.Size.minTapTarget)
        .accessibilityIdentifier("settings.routing.tools.\(tool.id)")
    }

    /// What the Claude Code row says about its sign-in. The account is the only
    /// detail ever printed; a verified credential without one is a token, and
    /// nothing about where it came from is drawn.
    private func authLine(_ auth: HarnessToolAuth?) -> String {
        guard let auth, auth.verified else { return "Not signed in" }
        if let account = auth.account?.trimmingCharacters(in: .whitespacesAndNewlines),
           !account.isEmpty {
            return "Signed in as \(account)"
        }
        return "Signed in (token present)"
    }

    private var caption: some View {
        Text("A coding turn goes to the configured harness when one is chosen; everything else stays on the model picked here or the daemon's own.")
            .font(Design.Typography.caption)
            .foregroundStyle(Design.Colors.secondaryForeground)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.vertical, Design.Spacing.xs)
    }

    /// The harness monitor, under the picker that chooses the harness. The count
    /// is the daemon's own `running`, so the row says "1 running" only when one
    /// is, and stays quiet otherwise.
    private var harnessMonitorRow: some View {
        Button {
            showHarnessMonitor = true
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: "waveform.path.ecg")
                    .font(.system(size: 14))
                    .foregroundStyle(Design.Brand.accent)
                    .frame(width: 20, alignment: .center)

                Text("Harness")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)

                Spacer(minLength: Design.Spacing.sm)

                if harnessMonitor.runningCount > 0 {
                    Text(harnessMonitor.runningCount == 1 ? "1 running" : "\(harnessMonitor.runningCount) running")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Brand.accent)
                }

                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            .frame(minHeight: Design.Size.minTapTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("settings.harness.monitor")
        .accessibilityLabel("Harness monitor")
    }

    private var sectionDivider: some View {
        Divider()
            .overlay(Design.Colors.divider)
    }

    // MARK: - Bindings

    /// The switch moves first and the daemon answers second; a refusal puts it
    /// back, so the row never claims a setting the daemon is not running.
    private var automaticBinding: Binding<Bool> {
        Binding(
            get: { isAutomatic },
            set: { chosen in
                guard chosen != isAutomatic else { return }
                let previous = isAutomatic
                isAutomatic = chosen
                Task { await storeAutomatic(chosen, revertingTo: previous) }
            }
        )
    }

    private var harnessBinding: Binding<String> {
        Binding(
            get: { harness },
            set: { chosen in
                guard chosen != harness else { return }
                let previous = harness
                harness = chosen
                Task { await storeHarness(chosen, revertingTo: previous) }
            }
        )
    }

    // MARK: - Network

    /// The daemon's device base, with or without its `/v1` suffix already on it.
    ///
    /// The paired base is stored twice in this app, once with the API prefix and
    /// once without, and a URL built from the wrong one asks for `/v1/v1/routing`
    /// - which the daemon answers with a 404 that reads like the route is
    /// missing. Joining here removes the guess.
    private var daemonBaseURLString: String? {
        guard var base = settingsStore.settings.relayConfiguration.activeBaseURLString
            ?? pairingStore.pairedRelayConfiguration?.baseURLString, !base.isEmpty
        else { return nil }
        base = base.trimmingCharacters(in: .whitespacesAndNewlines)
        while base.hasSuffix("/") { base.removeLast() }
        if base.hasSuffix("/v1") { base.removeLast(3) }
        return base.isEmpty ? nil : base
    }

    private var routingBaseURL: URL? {
        daemonURL("/v1/routing")
    }

    /// Join a `/v1/...` route onto the same stripped base `routingBaseURL` uses,
    /// so the routing read, the tools read and the install POST cannot disagree
    /// about whether the paired URL already ends in `/v1`.
    /// Join a `/v1/...` route onto the paired base URL.
    ///
    /// Build 242: the paired URL is a relay URL and already ends in `/v1`, while
    /// every route path here starts with `/v1` too - so naively concatenating
    /// them asked for `/v1/v1/harness/tools/...` and the sign-in popup could
    /// never reach the daemon at all. One trailing segment is dropped, and any
    /// trailing slash with it.
    private func daemonURL(_ route: String) -> URL? {
        guard var base = daemonBaseURLString else { return nil }
        while base.hasSuffix("/") { base = String(base.dropLast()) }
        if base.hasSuffix("/v1") { base = String(base.dropLast(3)) }
        return URL(string: base + route)
    }

    /// The same base URL and token source the rest of this card uses, wrapped
    /// for the harness model.
    private func harnessDaemon() -> HarnessMonitorDaemon {
        HarnessMonitorDaemon.current(
            settingsStore: settingsStore,
            pairingStore: pairingStore,
            sessionStore: sessionStore
        )
    }

    private func loadRouting() async {
        guard !hasLoaded else { return }
        guard let base = routingBaseURL else {
            routingError = "No daemon is paired, so there is no routing to read."
            hasLoaded = true
            return
        }
        var request = URLRequest(url: base)
        if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        do {
            var (data, response) = try await URLSession.shared.data(for: request)
            // A 401 here is an expired access token, not a refusal: refresh once
            // and ask again before saying anything about the daemon.
            if let http = response as? HTTPURLResponse, http.statusCode == 401 {
                await sessionStore.refreshAccessTokenIfNeeded()
                if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
                    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                    (data, response) = try await URLSession.shared.data(for: request)
                }
            }
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
                let status = (response as? HTTPURLResponse)?.statusCode ?? -1
                // The same wording a refused write gets: a 403 here is the same
                // pairing limit, and this phone can no more read routing than
                // change it.
                routingError = refusal(status: status, body: data)
                hasLoaded = true
                return
            }
            apply(try JSONDecoder().decode(RoutingPayload.self, from: data))
        } catch {
            routingError = "The routing could not be read: \(error.localizedDescription)"
            hasLoaded = true
        }
    }

    /// One read of `GET /v1/harness/tools`.
    ///
    /// A 404 is the daemon saying it does not serve the read: that is a state,
    /// not a failure, and it leaves the rows hidden. Any other failure hides
    /// them too, because a status dot that no answer backs would be a guess.
    private func loadHarnessTools() async {
        guard let url = daemonURL("/v1/harness/tools") else {
            harnessTools = []
            return
        }
        var request = URLRequest(url: url)
        request.timeoutInterval = 15
        if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        do {
            var (data, response) = try await URLSession.shared.data(for: request)
            if let http = response as? HTTPURLResponse, http.statusCode == 401 {
                await sessionStore.refreshAccessTokenIfNeeded()
                if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
                    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                    (data, response) = try await URLSession.shared.data(for: request)
                }
            }
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
                harnessTools = []
                return
            }
            harnessTools = try JSONDecoder().decode(HarnessToolsPayload.self, from: data).tools
        } catch {
            harnessTools = []
        }
    }

    /// `POST /v1/harness/tools/{id}/install`.
    ///
    /// The daemon owns what "install" means; this only reports what it said. A
    /// 400 or 409 carries the refusal text, including the hint when the tool
    /// cannot be installed here. Either way the rows are re-read, because an
    /// install can take a moment and the next row should draw what is true now.
    private func install(_ tool: HarnessTool) async {
        guard installingToolID == nil else { return }
        installingToolID = tool.id
        defer { installingToolID = nil }
        guard let url = daemonURL("/v1/harness/tools/\(Self.pathComponent(tool.id))/install") else {
            routingError = "No daemon is paired, so there is no harness to install."
            return
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 30
        if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        do {
            var (data, response) = try await URLSession.shared.data(for: request)
            if let http = response as? HTTPURLResponse, http.statusCode == 401 {
                await sessionStore.refreshAccessTokenIfNeeded()
                if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
                    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                    (data, response) = try await URLSession.shared.data(for: request)
                }
            }
            guard let http = response as? HTTPURLResponse else {
                routingError = "The harness could not be installed: the daemon sent no answer."
                return
            }
            if (200..<300).contains(http.statusCode) {
                routingError = nil
            } else {
                routingError = refusal(status: http.statusCode, body: data)
            }
            await loadHarnessTools()
        } catch {
            routingError = "The harness could not be installed: \(error.localizedDescription)"
        }
    }

    /// The Verify row action: no write, only the read the row already draws.
    private func verifyHarnessTools() async {
        guard !isVerifyingTools else { return }
        isVerifyingTools = true
        defer { isVerifyingTools = false }
        await loadHarnessTools()
    }

    private static func pathComponent(_ value: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }

    /// One PUT against `/v1/routing`. Both the full save and the two
    /// single-field writes go through here, so the 401 rule and the refusal
    /// wording exist once.
    private func putRouting(_ payload: [String: Any]) async -> RoutingPutOutcome {
        guard let base = routingBaseURL else { return .offline }
        var request = URLRequest(url: base)
        request.httpMethod = "PUT"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        request.httpBody = try? JSONSerialization.data(withJSONObject: payload)
        do {
            var (data, response) = try await URLSession.shared.data(for: request)
            if let http = response as? HTTPURLResponse, http.statusCode == 401 {
                await sessionStore.refreshAccessTokenIfNeeded()
                if let token = await sessionStore.currentAccessToken(), !token.isEmpty {
                    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                    (data, response) = try await URLSession.shared.data(for: request)
                }
            }
            guard let http = response as? HTTPURLResponse else {
                return .failed(reason: "the daemon sent no answer")
            }
            guard (200..<300).contains(http.statusCode) else {
                return .refused(status: http.statusCode, body: data)
            }
            return .stored(try? JSONDecoder().decode(RoutingPayload.self, from: data))
        } catch {
            return .failed(reason: error.localizedDescription)
        }
    }

    // MARK: - Writes

    private func saveModels() async {
        guard !isSaving else { return }
        isSaving = true
        defer { isSaving = false }
        // A model the chosen harness does not list is refused by the daemon. The
        // tools read is what knows the list, so when it has answered for this
        // harness, clear the value here and say why instead of sending a body
        // that is already a 400. When the read has not answered (404, offline)
        // nothing is known, so the value is left for the daemon to judge.
        var harnessModelNote: String?
        if harness != "none",
           let tool = harnessTools.first(where: { $0.id == harness }) {
            let chosen = trimmed(harnessModel)
            if !chosen.isEmpty, !tool.models.contains(where: { $0.id == chosen }) {
                harnessModelNote = "The chosen harness does not support \(chosen), so it was cleared."
                harnessModel = ""
            }
        }
        let payload: [String: Any] = [
            "auto": isAutomatic,
            "coding_model": trimmed(codingModel),
            "heavy_model": trimmed(heavyModel),
            "general_model": trimmed(generalModel),
            "harness_model": trimmed(harnessModel),
        ]
        let outcome = await putRouting(payload)
        switch outcome {
        case .stored(let routing):
            // A cleared model keeps its explanation in the same line a refusal
            // would use, so the row does not look as if nothing happened.
            routingError = harnessModelNote
            // The echoed record is what the daemon actually stored, so the
            // fields settle on it instead of on what was typed.
            if let routing {
                applyModels(routing)
            }
        case .refused(let status, let body):
            routingError = refusal(status: status, body: body)
        case .failed(let reason):
            routingError = "The routing could not be stored: \(reason)"
        case .offline:
            routingError = "No daemon is paired, so there is no routing to store."
        }
    }

    private func storeAutomatic(_ value: Bool, revertingTo previous: Bool) async {
        let outcome = await putRouting(["auto": value])
        switch outcome {
        case .stored(let routing):
            routingError = nil
            if let routing {
                isAutomatic = routing.auto
            }
        case .refused(let status, let body):
            routingError = refusal(status: status, body: body)
            isAutomatic = previous
        case .failed(let reason):
            routingError = "The routing could not be stored: \(reason)"
            isAutomatic = previous
        case .offline:
            routingError = "No daemon is paired, so there is no routing to store."
            isAutomatic = previous
        }
    }

    private func storeHarness(_ value: String, revertingTo previous: String) async {
        guard !isStoringHarness else {
            harness = previous
            return
        }
        isStoringHarness = true
        defer { isStoringHarness = false }
        let outcome = await putRouting(["harness": value])
        switch outcome {
        case .stored(let routing):
            routingError = nil
            if let routing {
                harness = routing.harness ?? value
                if let offered = routing.harnesses, !offered.isEmpty {
                    harnesses = offered
                }
            }
        case .refused(let status, let body):
            routingError = refusal(status: status, body: body)
            harness = previous
        case .failed(let reason):
            routingError = "The harness could not be stored: \(reason)"
            harness = previous
        case .offline:
            routingError = "No daemon is paired, so there is no harness to store."
            harness = previous
        }
    }

    // MARK: - Presentation helpers

    private func apply(_ routing: RoutingPayload) {
        isAutomatic = routing.auto
        applyModels(routing)
        harness = routing.harness ?? "none"
        harnesses = routing.harnesses ?? []
        hasLoaded = true
        routingError = nil
    }

    private func applyModels(_ routing: RoutingPayload) {
        codingModel = routing.codingModel ?? ""
        heavyModel = routing.heavyModel ?? ""
        generalModel = routing.generalModel ?? ""
        harnessModel = routing.harnessModel ?? ""
        if let models = routing.models {
            routingModels = models
        }
    }

    /// `none` first, then the daemon's own list, so clearing a harness is
    /// always one pick away.
    private var harnessOptions: [String] {
        var options: [String] = ["none"]
        for id in harnesses where !id.isEmpty && !options.contains(id) {
            options.append(id)
        }
        return options
    }

    private func harnessLabel(_ id: String) -> String {
        switch id {
        case "none": return "None"
        case "dsh": return "DSH"
        case "claude-code": return "Claude Code"
        case "antigravity": return "Antigravity"
        default: return id
        }
    }

    /// The daemon's refusal in words. A 403 is a pairing limit, not a bad
    /// value: this phone cannot read or change routing at all, which is the one
    /// thing worth saying. Anything else names the code and the status, because
    /// the code is the part that can be looked up.
    private func refusal(status: Int, body: Data) -> String {
        if status == 403 {
            return "This phone was paired without administration, so the daemon refuses routing changes."
        }
        var code = "unknown"
        var message = ""
        if let object = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] {
            if let raw = object["code"] {
                code = String(describing: raw)
            }
            if let raw = object["message"] as? String, !raw.isEmpty {
                message = raw
            }
        }
        let refused = "The daemon refused (\(code), \(status))."
        return message.isEmpty ? refused : "\(refused) \(message)"
    }

    private func trimmed(_ value: String) -> String {
        value.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
