import SwiftUI

// MARK: - Sidebar Section

/// Sections available in the iPad sidebar.
enum SidebarSection: String, CaseIterable, Identifiable {
    case chat
    case inbox
    case talk
    case notes
    case settings

    var id: String { rawValue }

    var title: String {
        switch self {
        case .chat:     "Chat"
        case .inbox:    "Inbox"
        case .talk:     "Talk"
        case .notes:    "Notes"
        case .settings: "Settings"
        }
    }

    var icon: String {
        switch self {
        case .chat:     "bubble.left.and.bubble.right"
        case .inbox:    "tray"
        case .talk:     "waveform"
        case .notes:    "pencil.and.outline"
        case .settings: "gearshape"
        }
    }
}

// MARK: - iPad Sidebar View

struct iPadSidebarView: View {
    @Binding var selectedSection: SidebarSection
    @Binding var isRightPanelOpen: Bool
    @Environment(HeraldHostStore.self) private var hostStore
    @Environment(SessionListStore.self) private var sessionStore
    @State private var renamingSession: SessionSummary?
    @State private var renameText = ""

    var body: some View {
        List {
            // ── Header ──
            Section {
                headerRow
            }

            // ── Session browser (only when Chat is selected or browsing) ──
            if selectedSection == .chat || sessionStore.searchResults != nil {
                // Search bar
                searchBar

                // Search results
                if let results = sessionStore.searchResults {
                    searchResultsSection(results)
                } else {
                    // Filter chips
                    filterChipsRow

                    // Cross-device toggle
                    allDevicesToggleRow

                    // Date-sectioned sessions
                    if sessionStore.filteredSessions.isEmpty {
                        emptyStateRow
                    } else {
                        ForEach(sessionStore.sessionSections) { section in
                            sessionDateSection(section)
                        }
                    }

                    // Platform sub-sections (only in "All" filter)
                    if sessionStore.activeFilter == .all {
                        platformSections
                    }

                    // Load more
                    if sessionStore.hasMore {
                        loadMoreRow
                    }
                }
            }

            // ── Bottom nav sections ──
            bottomSections
        }
        .listStyle(.sidebar)
        .scrollContentBackground(.hidden)
        .background(Design.Colors.background)
        .task { await sessionStore.loadSessions() }
        .alert("Rename Session", isPresented: .init(
            get: { renamingSession != nil },
            set: { if !$0 { renamingSession = nil } }
        )) {
            TextField("Title", text: $renameText)
            Button("Rename") {
                if let session = renamingSession {
                    Task { await sessionStore.renameSession(session, newTitle: renameText) }
                }
                renamingSession = nil
            }
            Button("Cancel", role: .cancel) { renamingSession = nil }
        }
        .alert("Error", isPresented: Binding(
            get: { sessionStore.errorMessage != nil },
            set: { if !$0 { sessionStore.errorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { sessionStore.errorMessage = nil }
        } message: {
            Text(sessionStore.errorMessage ?? "")
        }
    }

    // MARK: - Header

    private var headerRow: some View {
        HStack {
            Text("Herald")
                .font(Design.Typography.screenTitle)
                .foregroundStyle(Design.Colors.foreground)
            Spacer()
            Button {
                Task {
                    await sessionStore.createNewSession()
                    selectedSection = .chat
                }
            } label: {
                Image(systemName: "square.and.pencil")
                    .font(.system(size: Design.Size.iconSmall))
                    .foregroundStyle(Design.Brand.accent)
            }
            .buttonStyle(.plain)
            .help("New chat session")

            Button {
                withAnimation(Design.Motion.standard) {
                    isRightPanelOpen.toggle()
                }
            } label: {
                Image(systemName: "sidebar.right")
                    .font(.system(size: Design.Size.iconSmall))
                    .foregroundStyle(
                        isRightPanelOpen ? Design.Brand.accent : Design.Colors.secondaryForeground
                    )
            }
            .buttonStyle(.plain)
            .help("Toggle inspector panel")
        }
        .padding(.vertical, Design.Spacing.xs)
    }

    // MARK: - Search Bar

    private var searchBar: some View {
        HStack(spacing: Design.Spacing.xs) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(Design.Colors.secondaryForeground)
                .font(.system(size: Design.Size.iconSmall))
            TextField("Search sessions\u{2026}", text: Bindable(sessionStore).searchQuery)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
                .textFieldStyle(.plain)
                .autocorrectionDisabled()
            if !sessionStore.searchQuery.isEmpty {
                Button {
                    sessionStore.searchQuery = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .font(.system(size: Design.Size.iconSmall))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.vertical, Design.Spacing.xxs)
    }

    // MARK: - Search Results

    private func searchResultsSection(_ results: [SessionSummary]) -> some View {
        Section("SEARCH RESULTS") {
            if results.isEmpty {
                Text("No results")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            } else {
                ForEach(results) { session in
                    sessionRow(session)
                }
            }
        }
    }

    // MARK: - Filter Chips

    private var filterChipsRow: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Design.Spacing.xs) {
                ForEach(SessionFilter.allCases) { filter in
                    Button {
                        withAnimation(Design.Motion.standard) {
                            sessionStore.activeFilter = filter
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: filter.icon)
                                .font(.system(size: 10))
                            Text(filter.rawValue)
                                .font(.system(size: 12, weight: .medium))
                        }
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                        .foregroundStyle(
                            sessionStore.activeFilter == filter
                                ? Design.Brand.accent
                                : Design.Colors.secondaryForeground
                        )
                        .background(
                            sessionStore.activeFilter == filter
                                ? Design.Brand.accent.opacity(0.12)
                                : Color.clear
                        )
                        .clipShape(Capsule())
                        .overlay(
                            Capsule().stroke(
                                sessionStore.activeFilter == filter
                                    ? Design.Brand.accent.opacity(0.3)
                                    : Design.Colors.secondaryForeground.opacity(0.2),
                                lineWidth: 1
                            )
                        )
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.xs)
        }
        .listRowInsets(EdgeInsets())
        .listRowBackground(Color.clear)
    }

    // MARK: - All Devices Toggle

    private var allDevicesToggleRow: some View {
        Toggle("All Devices", isOn: Bindable(sessionStore).showAllDevices)
            .font(.caption)
            .tint(Design.Brand.accent)
            .padding(.horizontal, Design.Spacing.md)
            .listRowBackground(Color.clear)
    }

    // MARK: - Date Section

    private func sessionDateSection(_ section: SessionSection) -> some View {
        Section(section.title.uppercased()) {
            ForEach(section.sessions) { session in
                sessionRow(session, showPinIndicator: session.isPinned)
            }
        }
    }

    // MARK: - Empty State

    private var emptyStateRow: some View {
        VStack(spacing: Design.Spacing.md) {
            Spacer().frame(height: 32)
            Image(systemName: sessionStore.activeFilter == .archived ? "archivebox" : "bubble.left.and.bubble.right")
                .font(.system(size: 28))
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.5))
            Text(sessionStore.activeFilter == .archived ? "No Archived Sessions" : "No Sessions Yet")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.secondaryForeground)
            Text(sessionStore.activeFilter == .archived
                 ? "Archived sessions will appear here."
                 : "Start a conversation to create your first session.")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .listRowBackground(Color.clear)
    }

    // MARK: - Platform Sub-Sections

    @ViewBuilder
    private var platformSections: some View {
        let groups = sessionStore.sessionsBySource
            .filter { $0.source != "hermes" && $0.source != "ios" }
        ForEach(groups, id: \.source) { group in
            Section(group.source.uppercased()) {
                ForEach(group.sessions.prefix(5)) { session in
                    sessionRow(session)
                }
            }
        }
    }

    // MARK: - Load More

    private var loadMoreRow: some View {
        Button {
            Task { await sessionStore.loadMore() }
        } label: {
            HStack {
                Spacer()
                if sessionStore.isLoading {
                    ProgressView()
                        .tint(Design.Colors.secondaryForeground)
                } else {
                    Text("Load more")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Brand.accent)
                }
                Spacer()
            }
            .padding(.vertical, Design.Spacing.xs)
        }
        .buttonStyle(.plain)
    }

    // MARK: - Session Row

    private func sessionRow(_ session: SessionSummary, showPinIndicator: Bool = false) -> some View {
        Button {
            selectedSection = .chat
            Task { await sessionStore.switchToSession(session) }
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                // Source icon
                Image(systemName: session.sourceIcon)
                    .font(.system(size: Design.Size.iconSmall))
                    .foregroundStyle(
                        sessionStore.activeSessionID == session.id
                            ? Design.Brand.accent
                            : Design.Colors.secondaryForeground
                    )
                    .frame(width: 20)

                // Text content
                VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
                    HStack(spacing: Design.Spacing.xxxs) {
                        Text(session.title)
                            .font(
                                sessionStore.activeSessionID == session.id
                                    ? Design.Typography.headline
                                    : Design.Typography.body
                            )
                            .foregroundStyle(
                                sessionStore.activeSessionID == session.id
                                    ? Design.Brand.accent
                                    : Design.Colors.foreground
                            )
                            .lineLimit(1)
                        if showPinIndicator {
                            Image(systemName: "pin.fill")
                                .font(.system(size: 8))
                                .foregroundStyle(Design.Brand.accent)
                                .rotationEffect(.degrees(45))
                        }
                    }
                    Text(session.previewText)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .lineLimit(1)
                }

                Spacer()

                // Relative timestamp
                Text(session.relativeTimeString)
                    .font(Design.Typography.caption2)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            .padding(.vertical, Design.Spacing.xxxs)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .listRowBackground(
            sessionStore.activeSessionID == session.id
                ? Design.Brand.accent.opacity(0.12)
                : Color.clear
        )
        .contextMenu {
            sessionContextMenu(session)
        }
        .swipeActions(edge: .leading, allowsFullSwipe: true) {
            Button {
                Task { await sessionStore.togglePin(session) }
            } label: {
                Label(session.isPinned ? "Unpin" : "Pin",
                      systemImage: session.isPinned ? "pin.slash" : "pin")
            }
            .tint(Design.Brand.accent)
        }
        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
            Button {
                Task { await sessionStore.archiveSession(session) }
            } label: {
                Label("Archive", systemImage: "archivebox")
            }
            .tint(.orange)
            Button(role: .destructive) {
                Task { await sessionStore.deleteSession(session) }
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
    }

    // MARK: - Context Menu

    @ViewBuilder
    private func sessionContextMenu(_ session: SessionSummary) -> some View {
        Button {
            Task { await sessionStore.togglePin(session) }
        } label: {
            Label(session.isPinned ? "Unpin" : "Pin",
                  systemImage: session.isPinned ? "pin.slash" : "pin")
        }

        Button {
            renameText = session.title
            renamingSession = session
        } label: {
            Label("Rename", systemImage: "pencil")
        }

        Button {
            Task { await sessionStore.archiveSession(session) }
        } label: {
            Label("Archive", systemImage: "archivebox")
        }

        Divider()

        Button(role: .destructive) {
            Task { await sessionStore.deleteSession(session) }
        } label: {
            Label("Delete", systemImage: "trash")
        }
    }

    // MARK: - Bottom Sections

    private var bottomSections: some View {
        Section {
            ForEach([SidebarSection.chat, .inbox, .talk, .notes, .settings], id: \.self) { section in
                Button {
                    selectedSection = section
                    sessionStore.searchQuery = ""
                } label: {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: section.icon)
                            .font(.system(size: Design.Size.iconSmall))
                            .foregroundStyle(selectedSection == section ? Design.Brand.accent : Design.Colors.secondaryForeground)
                            .frame(width: 24)
                        Text(section.title)
                            .font(Design.Typography.body)
                            .foregroundStyle(Design.Colors.foreground)
                        Spacer()
                        if section == .inbox && hostStore.connectionState != .online {
                            Circle()
                                .fill(.orange)
                                .frame(width: 8, height: 8)
                        }
                    }
                    .padding(.vertical, Design.Spacing.xs)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .listRowBackground(
                    selectedSection == section
                        ? Design.Brand.accent.opacity(0.12)
                        : Color.clear
                )
            }

            // ── Skills browser ──
            NavigationLink {
                SkillsBrowserView()
            } label: {
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "wrench.and.screwdriver")
                        .font(.system(size: Design.Size.iconSmall))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .frame(width: 24)
                    Text("Skills")
                        .font(Design.Typography.body)
                        .foregroundStyle(Design.Colors.foreground)
                }
                .padding(.vertical, Design.Spacing.xs)
            }

            // ── Cron jobs ──
            NavigationLink {
                CronManagerView()
            } label: {
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "clock.badge")
                        .font(.system(size: Design.Size.iconSmall))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .frame(width: 24)
                    Text("Cron Jobs")
                        .font(Design.Typography.body)
                        .foregroundStyle(Design.Colors.foreground)
                }
                .padding(.vertical, Design.Spacing.xs)
            }
        }
    }
}
