import Foundation
import Combine

// MARK: - Session Filter

enum SessionFilter: String, CaseIterable, Identifiable {
    case all = "All"
    case pinned = "Pinned"
    case archived = "Archived"

    var id: String { rawValue }

    var icon: String {
        switch self {
        case .all:      "line.3.horizontal.decrease.circle"
        case .pinned:   "pin"
        case .archived: "archivebox"
        }
    }
}

// MARK: - Session Section

struct SessionSection: Identifiable {
    let id: String
    let title: String
    let sessions: [SessionSummary]
}

// MARK: - Session List Store

@MainActor
@Observable
final class SessionListStore {
    var pinnedSessions: [SessionSummary] = []
    var recentSessions: [SessionSummary] = []
    var archivedSessions: [SessionSummary] = []
    var searchResults: [SessionSummary]?
    var isLoading = false
    var searchQuery = ""
    var activeFilter: SessionFilter = .all
    var errorMessage: String?

    /// Total session count from last fetch (for pagination).
    private var totalCount = 0
    /// Current page offset for pagination.
    private var currentOffset = 0
    /// Page size for load-more pagination.
    private let pageSize = 50
    /// Whether more sessions are available to load.
    var hasMore: Bool { currentOffset < totalCount }

    /// Whether to include sessions from every device on the account rather than
    /// just this device's (+ user-scoped) sessions. Backed by `UserSettings` via
    /// `settingsStore` so the preference persists across launches.
    var showAllDevices: Bool {
        get { settingsStore.settings.showAllDevices }
        set {
            guard newValue != settingsStore.settings.showAllDevices else { return }
            settingsStore.settings.showAllDevices = newValue
            Task { await loadSessions(forceRefresh: true) }
        }
    }

    private let heraldClient: any HeraldClientProtocol
    private let chatStore: ChatStore
    private let settingsStore: SettingsStore
    private let persistence: any AppPersistenceStoreProtocol
    private var searchTask: Task<Void, Never>?
    private var searchObservationTask: Task<Void, Never>?
    private var loadTask: Task<Void, Never>?

    /// Timestamp of last successful load for freshness checking.
    private var lastLoadAt: Date?
    /// Freshness interval — skip network if loaded within this window.
    private let freshnessInterval: TimeInterval = 30

    init(heraldClient: any HeraldClientProtocol, chatStore: ChatStore, settingsStore: SettingsStore, persistence: any AppPersistenceStoreProtocol) {
        self.heraldClient = heraldClient
        self.chatStore = chatStore
        self.settingsStore = settingsStore
        self.persistence = persistence
        observeSearchQuery()
        loadCachedSessions()
    }

    // MARK: - Load Sessions

    func loadSessions(forceRefresh: Bool = false) async {
        // Suppress in-flight loads unless forced
        guard forceRefresh || loadTask == nil else { return }

        // Check freshness unless forced
        if !forceRefresh,
           let lastLoadAt,
           Date().timeIntervalSince(lastLoadAt) < freshnessInterval {
            return
        }

        loadTask?.cancel()
        loadTask = Task { await performLoad() }
        await loadTask?.value
    }

    private func performLoad() async {
        isLoading = true
        errorMessage = nil
        defer {
            isLoading = false
            loadTask = nil
        }

        do {
            let response = try await heraldClient.listSessions(limit: pageSize, offset: 0, allDevices: showAllDevices)
            currentOffset = response.sessions.count
            totalCount = response.total
            splitSessions(response.sessions)
            lastLoadAt = Date()
            saveCachedSessions()
        } catch {
            // Don't clear existing sessions on error
            errorMessage = error.localizedDescription
        }
    }

    func loadMore() async {
        guard hasMore, !isLoading else { return }
        isLoading = true
        defer { isLoading = false }

        do {
            let response = try await heraldClient.listSessions(limit: pageSize, offset: currentOffset, allDevices: showAllDevices)
            currentOffset += response.sessions.count
            // Merge new sessions and re-split
            let allSessions = pinnedSessions + recentSessions + response.sessions
            splitSessions(allSessions)
            saveCachedSessions()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: - Search

    func search() async {
        let query = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            searchResults = nil
            return
        }

        do {
            let results = try await heraldClient.searchSessions(query: query, allDevices: showAllDevices)
            searchResults = results
        } catch {
            errorMessage = error.localizedDescription
            searchResults = []
        }
    }

    // MARK: - Session Actions

    /// Update the title of a session in all local lists (pinned, recent, search, archived)
    /// and persist the cache. Called when the server derives or renames a title.
    func updateSessionTitle(id: UUID, newTitle: String) {
        if let idx = pinnedSessions.firstIndex(where: { $0.id == id }) {
            pinnedSessions[idx].title = newTitle
        }
        if let idx = recentSessions.firstIndex(where: { $0.id == id }) {
            recentSessions[idx].title = newTitle
        }
        if let idx = archivedSessions.firstIndex(where: { $0.id == id }) {
            archivedSessions[idx].title = newTitle
        }
        if let idx = searchResults?.firstIndex(where: { $0.id == id }) {
            searchResults?[idx].title = newTitle
        }
        saveCachedSessions()
    }

    func createNewSession(title: String = "New Chat") async {
        do {
            let session = try await heraldClient.createSession(title: title)
            recentSessions.insert(session, at: 0)
            await switchToSession(session)
            saveCachedSessions()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func switchToSession(_ session: SessionSummary) async {
        do {
            let conversation = try await heraldClient.loadConversation(id: session.id)
            chatStore.conversation = conversation
            if let latestUsage = conversation.latestUsage {
                chatStore.lastTokenUsage = latestUsage
            }
            chatStore.onConversationChanged?()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func deleteSession(_ session: SessionSummary) async {
        do {
            try await heraldClient.deleteSession(id: session.id)
            pinnedSessions.removeAll { $0.id == session.id }
            recentSessions.removeAll { $0.id == session.id }
            archivedSessions.removeAll { $0.id == session.id }
            searchResults?.removeAll { $0.id == session.id }
            saveCachedSessions()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func archiveSession(_ session: SessionSummary) async {
        do {
            try await heraldClient.archiveSession(id: session.id)
            pinnedSessions.removeAll { $0.id == session.id }
            recentSessions.removeAll { $0.id == session.id }
            searchResults?.removeAll { $0.id == session.id }
            // Move to archived list
            var archivedCopy = session
            archivedCopy.isArchived = true
            archivedSessions.insert(archivedCopy, at: 0)
            saveCachedSessions()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func togglePin(_ session: SessionSummary) async {
        do {
            let updated = try await heraldClient.togglePinSession(id: session.id)
            // Remove from current location
            pinnedSessions.removeAll { $0.id == session.id }
            recentSessions.removeAll { $0.id == session.id }
            // Re-insert in correct bucket
            if updated.isPinned {
                pinnedSessions.append(updated)
                pinnedSessions.sort { $0.lastActivity > $1.lastActivity }
            } else {
                recentSessions.append(updated)
                recentSessions.sort { $0.lastActivity > $1.lastActivity }
            }
            // Update search results if visible
            if let idx = searchResults?.firstIndex(where: { $0.id == session.id }) {
                searchResults?[idx] = updated
            }
            saveCachedSessions()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func renameSession(_ session: SessionSummary, newTitle: String) async {
        do {
            let updated = try await heraldClient.renameSession(id: session.id, title: newTitle)
            if let idx = pinnedSessions.firstIndex(where: { $0.id == session.id }) {
                pinnedSessions[idx] = updated
            }
            if let idx = recentSessions.firstIndex(where: { $0.id == session.id }) {
                recentSessions[idx] = updated
            }
            if let idx = searchResults?.firstIndex(where: { $0.id == session.id }) {
                searchResults?[idx] = updated
            }
            saveCachedSessions()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: - Helpers

    /// The active conversation's session ID, if any.
    var activeSessionID: UUID? {
        chatStore.conversation?.id
    }

    /// All sessions grouped by source for platform sub-sections.
    var sessionsBySource: [(source: String, sessions: [SessionSummary])] {
        let all = recentSessions
        let grouped = Dictionary(grouping: all) { $0.source ?? "herald" }
        return grouped
            .sorted { $0.key < $1.key }
            .map { (source: $0.key, sessions: $0.value) }
    }

    /// Sessions matching the active filter.
    var filteredSessions: [SessionSummary] {
        switch activeFilter {
        case .all:
            return pinnedSessions + recentSessions
        case .pinned:
            return pinnedSessions
        case .archived:
            return archivedSessions
        }
    }

    /// Sessions grouped into date-based sections for display.
    var sessionSections: [SessionSection] {
        let calendar = Calendar.current
        let sessions = filteredSessions

        guard !sessions.isEmpty else { return [] }

        let grouped = Dictionary(grouping: sessions) { session -> String in
            if calendar.isDateInToday(session.lastActivity) { return "Today" }
            if calendar.isDateInYesterday(session.lastActivity) { return "Yesterday" }
            if calendar.isDate(session.lastActivity, equalTo: Date(), toGranularity: .weekOfYear) { return "This Week" }
            return "Older"
        }

        let order = ["Today", "Yesterday", "This Week", "Older"]
        return order.compactMap { key in
            guard let sectionSessions = grouped[key], !sectionSessions.isEmpty else { return nil }
            let sorted = sectionSessions.sorted { $0.lastActivity > $1.lastActivity }
            return SessionSection(id: key, title: key, sessions: sorted)
        }
    }

    func reset() {
        pinnedSessions = []
        recentSessions = []
        archivedSessions = []
        searchResults = nil
        isLoading = false
        searchQuery = ""
        activeFilter = .all
        errorMessage = nil
        totalCount = 0
        currentOffset = 0
        lastLoadAt = nil
        searchTask?.cancel()
        searchTask = nil
        loadTask?.cancel()
        loadTask = nil
    }

    // MARK: - Private

    private func splitSessions(_ sessions: [SessionSummary]) {
        let nonArchived = sessions.filter { !$0.isArchived }
        pinnedSessions = nonArchived.filter(\.isPinned).sorted { $0.lastActivity > $1.lastActivity }
        recentSessions = nonArchived.filter { !$0.isPinned }.sorted { $0.lastActivity > $1.lastActivity }
        archivedSessions = sessions.filter(\.isArchived).sorted { $0.lastActivity > $1.lastActivity }
    }

    private func observeSearchQuery() {
        searchObservationTask = Task { [weak self] in
            guard let self else { return }
            var lastQuery = ""
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(100))
                let current = self.searchQuery
                guard current != lastQuery else { continue }
                lastQuery = current
                // Debounce 300ms
                try? await Task.sleep(for: .milliseconds(300))
                guard !Task.isCancelled, self.searchQuery == current else { continue }
                await self.search()
            }
        }
    }

    // MARK: - Session Cache

    private func loadCachedSessions() {
        guard let cached = persistence.loadSessionCache() else { return }
        splitSessions(cached)
    }

    private func saveCachedSessions() {
        let allSessions = pinnedSessions + recentSessions + archivedSessions
        persistence.saveSessionCache(allSessions)
    }
}
