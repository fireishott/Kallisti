import Foundation
import SwiftUI

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
