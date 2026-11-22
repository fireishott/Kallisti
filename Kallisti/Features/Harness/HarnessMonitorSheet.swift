//
//  HarnessMonitorSheet.swift
//  Kallisti
//
//  What the daemon's coding harnesses are doing: the runs it has open, the tail
//  of one expanded run, and the watchdog's account of how long that run has been
//  silent.
//
//  Two entry points draw this same sheet - a row in the Routing card, and the
//  banner above the composer while something is running. `HarnessMonitorModel`
//  is the one shared view model for both: ChatScreen owns an instance and hands
//  it to the sheet through the environment, and the Settings row owns one of its
//  own (the two screens are never on screen at the same time, so one instance per
//  presentation site is enough and keeps the app container untouched).
//
//  The list is polled every two seconds while the sheet is open, and the
//  expanded run's output is re-read on the same beat. The phone deliberately
//  does not subscribe to `GET /v1/harness/events`: a monitor is opened for a
//  minute, and a bounded poll gives the same rows without a second streaming
//  credential/refresh path or an SSE lifetime to clean up on suspend.
//

import Foundation
import SwiftUI

// MARK: - Wire types

/// One row of `GET /v1/harness/runs`, and the summary half of a single-run
/// read. `finished_at` travels too and is deliberately not decoded: the rows
/// draw elapsed and silent time, and only `started_at` is read, so the list can
/// be ordered newest first.
struct HarnessRun: Decodable, Identifiable {
    let id: String
    let harness: String
    let status: String
    let task: String?
    let elapsedMS: Int?
    let silentMS: Int?
    let stalled: Bool?
    let startedAt: HarnessStamp?

    enum CodingKeys: String, CodingKey {
        case id
        case harness
        case status
        case task
        case elapsedMS = "elapsed_ms"
        case silentMS = "silent_ms"
        case stalled
        case startedAt = "started_at"
    }
}

extension HarnessRun {
    /// The status the pill prints. The watchdog reports `stalled` as a flag
    /// while the status may still read `running`; the pill shows the louder of
    /// the two so a silent run does not look healthy.
    var pillStatus: String {
        if stalled == true, status == "running" { return "stalled" }
        return status
    }

    /// A run the person can still stop. The kill route only exists for a run
    /// the daemon still owns, so the button follows the status, not the flag.
    var isLive: Bool {
        status == "running" || status == "stalled"
    }
}

/// The single-run read. The summary fields repeat in this body and are not
/// decoded again: the row already holds them, so only the three fields the
/// expanded row draws are read here.
struct HarnessRunDetail: Decodable {
    let stdout: String?
    let stderr: String?
    let timeoutSecs: Int?
    let exitCode: Int?

    enum CodingKeys: String, CodingKey {
        case stdout
        case stderr
        case timeoutSecs = "timeout_secs"
        case exitCode = "exit_code"
    }
}

private struct HarnessRunListPayload: Decodable {
    let running: Int
    let runs: [HarnessRun]
}

private struct HarnessRunDetailPayload: Decodable {
    let run: HarnessRunDetail
}

/// `started_at` is read for ordering only, and accepted as either an ISO-8601
/// string or an epoch number: the writer of that field is not fixed by the
/// contract, and a stamp this build cannot parse must not take the row down.
struct HarnessStamp: Decodable {
    let seconds: Double?

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let number = try? container.decode(Double.self) {
            seconds = number
        } else if let text = try? container.decode(String.self) {
            seconds = ISO8601DateFormatter().date(from: text)?.timeIntervalSince1970
        } else {
            seconds = nil
        }
    }
}

// MARK: - Credentials

/// Which daemon to ask, and how to prove this phone may. Built from the stores
/// a view already has, so nothing about pairing or tokens is re-derived here.
struct HarnessMonitorDaemon {
    let baseURLString: String?
    let accessToken: @MainActor () async -> String?
    let refreshAccessToken: @MainActor () async -> Void

    /// The same base URL and token the Routing card uses: the user-entered
    /// relay wins, the paired relay is the fallback, and the token comes from
    /// the session store's pairing.
    @MainActor
    static func current(
        settingsStore: SettingsStore,
        pairingStore: PairingStore,
        sessionStore: AppSessionStore
    ) -> HarnessMonitorDaemon {
        HarnessMonitorDaemon(
            baseURLString: settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? pairingStore.pairedRelayConfiguration?.baseURLString,
            accessToken: { await sessionStore.currentAccessToken() },
            refreshAccessToken: { await sessionStore.refreshAccessTokenIfNeeded() }
        )
    }
}

// MARK: - Model

/// Who is keeping the list poll open. The chat banner and the sheet share one
/// loop, so the loop runs while either of them is on screen and stops when the
/// last one goes away.
enum HarnessMonitorPollHolder: Hashable {
    case chat
    case sheet
}

private enum HarnessHTTPOutcome {
    case answered(status: Int, body: Data)
    case failed(String)
}

/// The rows, the expanded run's output, and the two polls that keep them
/// current. Everything here is main-actor state; the requests are the only
/// things that leave it.
@MainActor
@Observable
final class HarnessMonitorModel {
    /// Every run the daemon listed, newest first.
    private(set) var runs: [HarnessRun] = []
    /// The count the daemon reported for `running`, not a filter over `runs`:
    /// a daemon that pages its history still counts every live run.
    private(set) var runningCount = 0
    /// A first answer of some kind has arrived. Until then the sheet shows a
    /// spinner rather than an empty state it cannot vouch for.
    private(set) var hasLoaded = false
    /// Set by a 404 from the list: the daemon has no harness monitor yet.
    private(set) var listIsMissing = false
    /// A failure to read or refresh, in words.
    private(set) var listMessage: String?
    private(set) var details: [String: HarnessRunDetail] = [:]
    private(set) var detailMessages: [String: String] = [:]
    private(set) var killMessages: [String: String] = [:]
    private(set) var killing: Set<String> = []

    private static let pollInterval: Duration = .seconds(2)
    private static let outputLimit = 4000

    private var activeDaemon: HarnessMonitorDaemon?
    private var holders: Set<HarnessMonitorPollHolder> = []
    private var listTask: Task<Void, Never>?
    private var detailTask: Task<Void, Never>?
    private var detailRunID: String?

    // MARK: Polling

    /// Start, or join, the two-second list poll. Calling this twice from the
    /// same holder is a no-op, so a view that re-appears cannot stack loops.
    func startListPolling(daemon: HarnessMonitorDaemon, for holder: HarnessMonitorPollHolder) {
        activeDaemon = daemon
        holders.insert(holder)
        guard listTask == nil else { return }
        listTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.refreshList()
                guard !Task.isCancelled else { return }
                try? await Task.sleep(for: Self.pollInterval)
            }
        }
    }

    func stopListPolling(for holder: HarnessMonitorPollHolder) {
        holders.remove(holder)
        guard holders.isEmpty else { return }
        listTask?.cancel()
        listTask = nil
    }

    /// Follow one expanded row. The first read happens immediately, then on the
    /// same two-second beat as the list.
    func startDetailPolling(runID: String, daemon: HarnessMonitorDaemon) {
        activeDaemon = daemon
        if detailRunID == runID, detailTask != nil { return }
        detailTask?.cancel()
        detailRunID = runID
        detailTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.refreshDetail(runID: runID)
                guard !Task.isCancelled else { return }
                try? await Task.sleep(for: Self.pollInterval)
            }
        }
    }

    func stopDetailPolling() {
        detailTask?.cancel()
        detailTask = nil
        detailRunID = nil
    }

    // MARK: Reads

    /// One read of `GET /v1/harness/runs`. A 404 is the daemon saying it has no
    /// harness monitor: that is a state, not a failure, and it clears any rows
    /// so the sheet never shows a stale list as if it were live.
    func refreshList(daemon overrideDaemon: HarnessMonitorDaemon? = nil) async {
        if let overrideDaemon { activeDaemon = overrideDaemon }
        guard let daemon = activeDaemon,
              let url = HarnessMonitorURL.route(daemon.baseURLString, "/v1/harness/runs")
        else {
            listIsMissing = false
            listMessage = "No daemon is paired, so there is no harness to monitor."
            hasLoaded = true
            return
        }
        let outcome = await perform(url, method: "GET", daemon: daemon)
        switch outcome {
        case .answered(let status, let body):
            if status == 404 {
                listIsMissing = true
                listMessage = nil
                runs = []
                runningCount = 0
                hasLoaded = true
                return
            }
            guard (200..<300).contains(status) else {
                listMessage = Self.refusal(status: status, body: body, action: "read the harness runs")
                hasLoaded = true
                return
            }
            do {
                let payload = try JSONDecoder().decode(HarnessRunListPayload.self, from: body)
                runs = Self.newestFirst(payload.runs)
                runningCount = payload.running
                listIsMissing = false
                listMessage = nil
            } catch {
                listMessage = "The harness runs could not be read: \(error.localizedDescription)"
            }
            hasLoaded = true
        case .failed(let reason):
            listMessage = "The harness runs could not be read: \(reason)"
            hasLoaded = true
        }
    }

    /// One read of `GET /v1/harness/runs/{id}`.
    func refreshDetail(runID: String) async {
        guard let daemon = activeDaemon,
              let url = HarnessMonitorURL.route(
                daemon.baseURLString,
                "/v1/harness/runs/\(Self.encoded(runID))"
              )
        else {
            detailMessages[runID] = "No daemon is paired, so there is no run to read."
            return
        }
        let outcome = await perform(url, method: "GET", daemon: daemon)
        switch outcome {
        case .answered(let status, let body):
            guard (200..<300).contains(status) else {
                detailMessages[runID] = Self.refusal(status: status, body: body, action: "read that run")
                return
            }
            do {
                let payload = try JSONDecoder().decode(HarnessRunDetailPayload.self, from: body)
                details[runID] = payload.run
                detailMessages[runID] = nil
            } catch {
                detailMessages[runID] = "The run could not be read: \(error.localizedDescription)"
            }
        case .failed(let reason):
            detailMessages[runID] = "The run could not be read: \(reason)"
        }
    }

    // MARK: Kill

    /// `POST /v1/harness/runs/{id}/kill`.
    ///
    /// A 403 here is the pairing limit, not a bad run: the phone was paired
    /// without administration, and that is worth saying in one sentence instead
    /// of a code. Every other refusal names the code and the status.
    func kill(runID: String) async {
        guard !killing.contains(runID) else { return }
        guard let daemon = activeDaemon,
              let url = HarnessMonitorURL.route(
                daemon.baseURLString,
                "/v1/harness/runs/\(Self.encoded(runID))/kill"
              )
        else {
            killMessages[runID] = "No daemon is paired, so there is no harness to stop."
            return
        }
        killing.insert(runID)
        defer { killing.remove(runID) }
        let outcome = await perform(url, method: "POST", daemon: daemon)
        switch outcome {
        case .answered(let status, let body):
            if (200..<300).contains(status) {
                killMessages[runID] = nil
                await refreshList()
                await refreshDetail(runID: runID)
            } else if status == 403 {
                killMessages[runID] = "This phone was paired without administration, so the daemon refuses to stop the harness."
            } else {
                killMessages[runID] = Self.refusal(status: status, body: body, action: "stop the harness")
            }
        case .failed(let reason):
            killMessages[runID] = "The harness could not be stopped: \(reason)"
        }
    }

    // MARK: Requests

    /// One request with the routing card's token rule: send the access token,
    /// and on a 401 refresh once and ask again before reporting a refusal.
    private func perform(
        _ url: URL,
        method: String,
        daemon: HarnessMonitorDaemon
    ) async -> HarnessHTTPOutcome {
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 15
        if let token = await daemon.accessToken(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        do {
            var (data, response) = try await URLSession.shared.data(for: request)
            if let http = response as? HTTPURLResponse, http.statusCode == 401 {
                await daemon.refreshAccessToken()
                if let token = await daemon.accessToken(), !token.isEmpty {
                    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                    (data, response) = try await URLSession.shared.data(for: request)
                }
            }
            guard let http = response as? HTTPURLResponse else {
                return .failed("the daemon sent no answer")
            }
            return .answered(status: http.statusCode, body: data)
        } catch {
            return .failed(error.localizedDescription)
        }
    }

    // MARK: Helpers

    /// Newest first, the order the rows are drawn in. A run whose stamp this
    /// build could not parse keeps the daemon's order instead of being dropped
    /// or sorted to an invented time.
    private static func newestFirst(_ runs: [HarnessRun]) -> [HarnessRun] {
        runs.enumerated()
            .sorted { left, right in
                switch (left.element.startedAt?.seconds, right.element.startedAt?.seconds) {
                case let (leftSeconds?, rightSeconds?) where leftSeconds != rightSeconds:
                    return leftSeconds > rightSeconds
                default:
                    return left.offset < right.offset
                }
            }
            .map(\.element)
    }

    private static func encoded(_ runID: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        return runID.addingPercentEncoding(withAllowedCharacters: allowed) ?? runID
    }

    /// The daemon's refusal in words. The body carries a `code` and a
    /// `message`; the code and the status are always named because they are the
    /// part that can be looked up.
    private static func refusal(status: Int, body: Data, action: String) -> String {
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
        let refused = "The daemon refused to \(action) (\(code), \(status))."
        return message.isEmpty ? refused : "\(refused) \(message)"
    }

    fileprivate static func cappedTail(_ text: String) -> (text: String, truncated: Bool) {
        guard text.count > outputLimit else { return (text, false) }
        return (String(text.suffix(outputLimit)), true)
    }

    fileprivate static var outputCharacterLimit: Int { outputLimit }
}

// MARK: - URLs

private enum HarnessMonitorURL {
    /// Join a `/v1/...` route onto the paired base URL.
    ///
    /// The paired URL is a relay URL and ends in `/v1`; the route paths start
    /// with `/v1` too, so one trailing segment is dropped instead of producing
    /// `/v1/v1/harness/runs`.
    static func route(_ baseURLString: String?, _ route: String) -> URL? {
        guard var base = baseURLString?.trimmingCharacters(in: .whitespacesAndNewlines),
              !base.isEmpty
        else { return nil }
        while base.hasSuffix("/") {
            base = String(base.dropLast())
        }
        if base.hasSuffix("/v1") {
            base = String(base.dropLast(3))
        }
        return URL(string: base + route)
    }
}

// MARK: - Sheet

/// The monitor itself. The rows come from the model, so the same two-second
/// poll that feeds the banner also redraws this list; the expanded row follows
/// its own run at the same cadence.
struct HarnessMonitorSheet: View {
    @Environment(HarnessMonitorModel.self) private var monitor
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(AppSessionStore.self) private var sessionStore

    let onDismiss: () -> Void

    @State private var expandedID: String?

    var body: some View {
        NavigationStack {
            List {
                if let listMessage = monitor.listMessage {
                    Section {
                        Text(listMessage)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.warning)
                            .fixedSize(horizontal: false, vertical: true)
                            .accessibilityIdentifier("harness.monitor.error")
                    }
                }

                if monitor.listIsMissing {
                    ContentUnavailableView(
                        "The daemon has no harness monitor yet.",
                        systemImage: "hammer",
                        description: Text("This daemon does not serve harness runs. Nothing is listed here until it does.")
                    )
                } else if monitor.runs.isEmpty {
                    // A failure already has its own row above; the empty state
                    // would only repeat that nothing was read.
                    if monitor.listMessage == nil {
                        emptyOrLoading
                    }
                } else {
                    ForEach(monitor.runs) { run in
                        row(run)
                    }
                }
            }
            .navigationTitle("Harness")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { onDismiss() }
                }
            }
        }
        .task {
            monitor.startListPolling(daemon: daemon(), for: .sheet)
        }
        .onDisappear {
            monitor.stopListPolling(for: .sheet)
            monitor.stopDetailPolling()
        }
        .onChange(of: expandedID) { _, id in
            if let id {
                monitor.startDetailPolling(runID: id, daemon: daemon())
            } else {
                monitor.stopDetailPolling()
            }
        }
        .onChange(of: monitor.runs.map(\.id)) { _, ids in
            // A run that dropped out of the list has no row to expand, so its
            // detail poll has nothing left to follow.
            if let current = expandedID, !ids.contains(current) {
                expandedID = nil
            }
        }
    }

    private func daemon() -> HarnessMonitorDaemon {
        HarnessMonitorDaemon.current(
            settingsStore: settingsStore,
            pairingStore: pairingStore,
            sessionStore: sessionStore
        )
    }

    // MARK: Rows

    @ViewBuilder
    private var emptyOrLoading: some View {
        if monitor.hasLoaded {
            ContentUnavailableView(
                "No harness runs",
                systemImage: "hammer",
                description: Text("A coding turn that hands work to a harness lists the run here while it works.")
            )
        } else {
            HStack(spacing: Design.Spacing.sm) {
                ProgressView()
                    .controlSize(.small)
                Text("Reading the harness runs.")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
    }

    private func row(_ run: HarnessRun) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            summary(run)
            if expandedID == run.id {
                detail(run)
            }
        }
        .padding(.vertical, Design.Spacing.xxs)
    }

    /// The collapsed row: harness, status pill, elapsed, silent time while it
    /// runs, the task preview, and the chevron. The whole block is the tap
    /// target; the expanded block below it is not, so the Kill button cannot
    /// toggle the row.
    @ViewBuilder
    private func summary(_ run: HarnessRun) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.sm) {
                Text(run.harness.isEmpty ? run.id : run.harness)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
                    .multilineTextAlignment(.leading)

                Spacer(minLength: Design.Spacing.sm)

                statusPill(run)

                Image(systemName: expandedID == run.id ? "chevron.down" : "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }

            HStack(spacing: Design.Spacing.md) {
                Text(harnessDuration(run.elapsedMS))
                if run.isLive, let silent = run.silentMS {
                    Text("silent \(harnessDuration(silent))")
                        .foregroundStyle(
                            run.stalled == true
                                ? Design.Colors.warning
                                : Design.Colors.secondaryForeground
                        )
                }
            }
            .font(.system(.caption2, design: .monospaced))
            .foregroundStyle(Design.Colors.secondaryForeground)

            if let task = run.task, !task.isEmpty {
                Text(task)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .lineLimit(expandedID == run.id ? 3 : 1)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture {
            if expandedID == run.id {
                expandedID = nil
            } else {
                expandedID = run.id
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(run.pillStatus), \(harnessDuration(run.elapsedMS)), \(run.task ?? "no task text")")
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("harness.monitor.row")
    }

    private func statusPill(_ run: HarnessRun) -> some View {
        let color = statusColor(run.pillStatus)
        return Text(run.pillStatus.uppercased())
            .font(.system(.caption2, design: .monospaced))
            .foregroundStyle(color)
            .lineLimit(1)
            .padding(.horizontal, Design.Spacing.xs)
            .padding(.vertical, Design.Spacing.xxxs)
            .background(Capsule().fill(color.opacity(0.12)))
            .overlay(
                Capsule()
                    .stroke(color.opacity(0.35), lineWidth: 1)
            )
            .accessibilityLabel("status \(run.pillStatus)")
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "running": return Design.Brand.accent
        case "stalled": return Design.Colors.warning
        case "succeeded": return Design.Colors.success
        case "failed", "timed_out": return Design.Colors.danger
        default: return Design.Colors.secondaryForeground
        }
    }

    // MARK: Expanded row

    @ViewBuilder
    private func detail(_ run: HarnessRun) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Divider()
                .overlay(Design.Colors.divider)

            if let detail = monitor.details[run.id] {
                outputSection(title: "stdout", text: detail.stdout, empty: "No output yet.")

                if let stderr = detail.stderr, !stderr.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    outputSection(title: "stderr", text: stderr, empty: "")
                }

                HStack(spacing: Design.Spacing.md) {
                    if let timeout = detail.timeoutSecs {
                        Text("timeout \(timeout)s")
                    }
                    if let code = detail.exitCode {
                        Text("exit \(code)")
                    }
                }
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(Design.Colors.secondaryForeground)
            } else if let message = monitor.detailMessages[run.id] {
                Text(message)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.warning)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                HStack(spacing: Design.Spacing.sm) {
                    ProgressView()
                        .controlSize(.small)
                    Text("Reading the run.")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            }

            if run.isLive {
                killButton(run)
            }

            if let killMessage = monitor.killMessages[run.id] {
                Text(killMessage)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.warning)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("harness.monitor.killError")
            }
        }
    }

    /// One capped, scrollable, monospaced block. The tail is kept rather than
    /// the head: a monitor is read for the newest output, and the notice below
    /// says what was dropped.
    @ViewBuilder
    private func outputSection(title: String, text: String?, empty: String) -> some View {
        let trimmed = (text ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            Text(title)
                .brandEyebrow()

            if trimmed.isEmpty {
                Text(empty)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            } else {
                let shown = HarnessMonitorModel.cappedTail(trimmed)
                ScrollView(.vertical) {
                    Text(shown.text)
                        .font(Design.Typography.codeSmall)
                        .foregroundStyle(Design.Colors.foreground)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 220)
                .padding(Design.Spacing.xs)
                .background(Design.Colors.background)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))

                if shown.truncated {
                    Text("Showing the last \(HarnessMonitorModel.outputCharacterLimit) characters.")
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            }
        }
    }

    private func killButton(_ run: HarnessRun) -> some View {
        let isKilling = monitor.killing.contains(run.id)
        return Button(role: .destructive) {
            Task { await monitor.kill(runID: run.id) }
        } label: {
            HStack(spacing: Design.Spacing.xs) {
                if isKilling {
                    ProgressView()
                        .controlSize(.small)
                }
                Text(isKilling ? "Killing…" : "Kill")
            }
        }
        .buttonStyle(.bordered)
        .tint(Design.Colors.danger)
        .disabled(isKilling)
        .accessibilityIdentifier("harness.monitor.kill")
        .accessibilityLabel(isKilling ? "Killing the run" : "Kill the run")
    }
}

// MARK: - Formatting

/// Elapsed and silent time in the same shape the subagent rows use, with an
/// hours form for a run that has been going long enough to need one.
private func harnessDuration(_ milliseconds: Int?) -> String {
    guard let milliseconds else { return "unknown" }
    let seconds = max(0, milliseconds / 1000)
    if seconds < 60 { return "\(seconds)s" }
    if seconds < 3600 { return String(format: "%d:%02d", seconds / 60, seconds % 60) }
    return String(format: "%dh %02dm", seconds / 3600, (seconds % 3600) / 60)
}
