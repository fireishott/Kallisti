import Foundation
import SwiftUI

/// What the daemon serves at `/v1/routing`, read for the parts the Routing card
/// draws. `ok` and `provider_model` travel too and are deliberately not decoded:
/// nothing here renders them.
private struct RoutingPayload: Decodable {
    let auto: Bool
    let codingModel: String?
    let heavyModel: String?
    let generalModel: String?
    let harness: String?
    let harnesses: [String]?

    enum CodingKeys: String, CodingKey {
        case auto
        case codingModel = "coding_model"
        case heavyModel = "heavy_model"
        case generalModel = "general_model"
        case harness
        case harnesses
    }
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
    /// `none`, or one of the daemon's harness ids.
    @State private var harness = "none"
    /// The harness ids the daemon offers, in its own order.
    @State private var harnesses: [String] = []
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
        .task {
            // One read on appearance is enough for the count here; the monitor
            // sheet it opens owns the two-second poll while it is on screen.
            await harnessMonitor.refreshList(daemon: harnessDaemon())
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
            modelField("Coding model", text: $codingModel)
            modelField("Heavy model", text: $heavyModel)
            modelField("General model", text: $generalModel)

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

    private func modelField(_ label: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
            Text(label)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)

            TextField("the daemon's own", text: text)
                .textInputAutocapitalization(.never)
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
                .disabled(!hasLoaded || isSaving)
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
    private var routingBaseURL: URL? {
        guard var base = settingsStore.settings.relayConfiguration.activeBaseURLString
            ?? pairingStore.pairedRelayConfiguration?.baseURLString, !base.isEmpty
        else { return nil }
        while base.hasSuffix("/") { base.removeLast() }
        if base.hasSuffix("/v1") { base.removeLast(3) }
        return URL(string: base + "/v1/routing")
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
        let payload: [String: Any] = [
            "auto": isAutomatic,
            "coding_model": trimmed(codingModel),
            "heavy_model": trimmed(heavyModel),
            "general_model": trimmed(generalModel),
        ]
        let outcome = await putRouting(payload)
        switch outcome {
        case .stored(let routing):
            routingError = nil
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
