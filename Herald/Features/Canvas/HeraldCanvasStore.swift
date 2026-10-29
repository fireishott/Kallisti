import SwiftUI
import Foundation

/// Manages the active Canvas artifact — extracted from messages or edited by the user.
/// Persisted to UserDefaults keyed by sessionID.
@Observable
@MainActor
final class HeraldCanvasStore {
    var activeArtifact: KallistiArtifact?

    /// Live tool activities from the current streaming message. Set by
    /// ChatStore when toolActivity events arrive. Drives the Live tab in
    /// the canvas panel (build 78).
    var liveToolActivities: [ToolActivity] = []

    /// Background processes tracked by the connector (build 135.17).
    /// Appended/merged as SSE events arrive from
    /// /v1/canvas/processes/stream. Rendered in the Live tab below the
    /// tool activities with a Stop button per running row.
    var liveBackgroundProcesses: [BackgroundProcess] = []

    /// Wired by AppContainer to the connector's process kill endpoint
    /// (build 135.17). CanvasView calls this when the user taps Stop on
    /// a running row. Nil = kill not wired (Stop hidden is handled by
    /// the row only showing the button while active, so a nil callback
    /// simply no-ops).
    var killBackgroundProcess: (@MainActor (String) -> Void)?

    /// Selected tab in the canvas panel. Auto-switches to .live when
    /// liveToolActivities becomes non-empty.
    var activeTab: CanvasTab = .artifact

    /// True when there is at least one live tool activity to show.
    var isLiveActivityVisible: Bool { !liveToolActivities.isEmpty }

    enum CanvasTab: Hashable { case artifact, live, subagents }

    private let defaults = UserDefaults.standard
    private let storageKey = "herald.canvas.artifacts"

    /// Load the stored artifact for a given session on startup.
    func loadArtifact(for sessionID: String) {
        guard let data = defaults.data(forKey: storageKey + "." + sessionID),
              let artifact = try? JSONDecoder().decode(KallistiArtifact.self, from: data)
        else { return }
        activeArtifact = artifact
    }

    /// Open an artifact extracted from a message's first code block.
    func open(message: Message, sessionID: String) {
        let segments = parseMarkdownSegments(message.content)
        guard let codeBlock = segments.first(where: {
            if case .codeBlock = $0 { return true }
            return false
        }), case .codeBlock(_, let lang, let code) = codeBlock else { return }

        let type: KallistiArtifactType = (lang?.lowercased() == "svg") ? .svg
            : lang.map { .code(language: $0) } ?? .markdown
        let artifact = KallistiArtifact(
            sessionID: sessionID,
            type: type,
            content: code
        )
        activeArtifact = artifact
        persist(artifact)
    }

    /// Called from CanvasView when user edits content.
    func updateContent(_ newContent: String) {
        guard var artifact = activeArtifact else { return }
        artifact.content = newContent
        artifact.updatedAt = Date()
        activeArtifact = artifact
        persist(artifact)
    }

    func clear() {
        if let sessionID = activeArtifact?.sessionID {
            defaults.removeObject(forKey: storageKey + "." + sessionID)
        }
        activeArtifact = nil
    }

    // MARK: - Background process feed (build 135.17)

    /// True when there is at least one background process row to show.
    var isProcessFeedVisible: Bool { !liveBackgroundProcesses.isEmpty }

    /// Merge an incoming SSE process event into the feed. Existing rows
    /// are updated in place (identity = process id); new ids are appended.
    /// Completed/failed rows stay listed so the user can read the tail,
    /// with the terminal status shown in the row.
    func upsertBackgroundProcess(_ process: BackgroundProcess) {
        if let idx = liveBackgroundProcesses.firstIndex(where: { $0.id == process.id }) {
            liveBackgroundProcesses[idx] = liveBackgroundProcesses[idx].merged(with: process)
        } else {
            liveBackgroundProcesses.append(process)
        }
    }

    /// Drop terminal rows that finished more than `cutoff` seconds ago
    /// so the Live tab does not accumulate finished processes forever.
    func pruneFinishedBackgroundProcesses(olderThan cutoff: TimeInterval = 120) {
        let now = Date()
        liveBackgroundProcesses.removeAll { process in
            guard !process.status.isActive else { return false }
            guard let finished = process.finishedAt ?? process.startedAt else { return true }
            return now.timeIntervalSince(finished) > cutoff
        }
    }

    private func persist(_ artifact: KallistiArtifact) {
        if let data = try? JSONEncoder().encode(artifact) {
            defaults.set(data, forKey: storageKey + "." + artifact.sessionID)
        }
    }
}

// MARK: - Subagents
/// The daemon's subagents, kept live for the chat toolbar and the canvas sheet.
///
/// One GET every few seconds while a surface is showing them: the daemon's list
/// is the dashboard bar's own view (working now, plus recently finished), so the
/// phone and the page cannot disagree about what is running. The badge is the
/// count of rows still `queued` or `running`, which is the number that has to
/// move as they start and finish.
@Observable
@MainActor
final class SubagentStore {
    /// Set by `AppContainer` when the app runs on Eris. Nil on the legacy relay,
    /// which has no subagents of its own.
    var erisProvider: (@MainActor () async throws -> ErisSubagentList)?

    /// Rows the daemon last reported, newest first.
    private(set) var rows: [ErisSubagent] = []
    /// The last read's failure, if it failed. The list keeps its last good rows.
    private(set) var lastError: String?
    /// True when the daemon bounded the list and older rows sit behind it.
    private(set) var truncated = false

    private var pollTask: Task<Void, Never>?
    private var refreshing = false

    /// What the toolbar badge shows: work queued or running right now.
    var activeCount: Int { rows.filter(\.isActive).count }

    /// Whether there is anything to show in the sheet at all.
    var hasRows: Bool { !rows.isEmpty }

    /// Read once. A poll and a manual refresh never stack.
    func refresh() async {
        guard let erisProvider, !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        do {
            let list = try await erisProvider()
            rows = list.subagents
            truncated = list.truncated
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// Poll while a surface is on screen. `startPolling` is idempotent, so an
    /// appear/disappear pair cannot leave two loops running.
    func startPolling(every seconds: TimeInterval = 3) {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            }
        }
    }

    func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }
}
