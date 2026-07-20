import SwiftUI

/// A slide-out session browser drawer for iPhone.
/// Uses a drag gesture to reveal/hide from the leading edge.
struct iPhoneSessionDrawer: View {
    @Environment(SessionListStore.self) private var sessionStore
    @Environment(TabRouter.self) private var router
    @Binding var isOpen: Bool
    @State private var dragOffset: CGFloat = 0

    var body: some View {
        GeometryReader { geometry in
            let drawerWidth = min(geometry.size.width * 0.82, 340)
            drawerBody(drawerWidth: drawerWidth)
        }
    }

    @ViewBuilder
    private func drawerBody(drawerWidth: CGFloat) -> some View {
        ZStack(alignment: .leading) {
            // Backdrop
            if isOpen {
                Color.black.opacity(0.35)
                    .ignoresSafeArea()
                    .onTapGesture { close() }
            }

            // Drawer content
            HStack(spacing: 0) {
                VStack(spacing: 0) {
                    drawerHeader
                    Divider()
                        .background(Design.Colors.divider)
                    sessionList
                }
                .frame(width: drawerWidth)
                .background(Design.Colors.background)
                .clipShape(
                    UnevenRoundedRectangle(
                        bottomTrailingRadius: Design.CornerRadius.lg,
                        topTrailingRadius: Design.CornerRadius.lg
                    )
                )

                Spacer()
            }
            .offset(x: isOpen ? min(0, dragOffset) : -drawerWidth + dragOffset)
            .animation(Design.Motion.standard, value: isOpen)
            .animation(.interactiveSpring, value: dragOffset)

            // Always-on-screen edge-catcher: the drawer HStack above is offset
            // off-screen when closed, so its own gesture is unreachable by touch.
            // This invisible strip along the leading edge stays in place and
            // carries the same drag gesture so swipe-to-open actually works.
            if !isOpen {
                Color.clear
                    .frame(width: 24)
                    .contentShape(Rectangle())
                    .frame(maxHeight: .infinity)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .gesture(
            DragGesture()
                .onChanged { value in
                    if isOpen {
                        // Only allow dragging closed (negative)
                        dragOffset = min(0, value.translation.width)
                    } else if value.translation.width > 10 {
                        // Allow peeking open from edge
                        dragOffset = value.translation.width
                    }
                }
                .onEnded { value in
                    let threshold = drawerWidth * 0.3
                    if isOpen {
                        if value.translation.width < -threshold {
                            close()
                        } else {
                            snapOpen()
                        }
                    } else {
                        if value.translation.width > threshold {
                            open()
                        } else {
                            snapClosed()
                        }
                    }
                }
        )
        .task {
            await sessionStore.loadSessions()
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

    private var drawerHeader: some View {
        HStack {
            Text("Sessions")
                .font(Design.Typography.screenTitle)
                .foregroundStyle(Design.Colors.foreground)
            Spacer()
            Button {
                Task { await sessionStore.createNewSession() }
            } label: {
                Image(systemName: "square.and.pencil")
                    .font(.system(size: Design.Size.iconSmall))
                    .foregroundStyle(Design.Brand.accent)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.md)
    }

    // MARK: - Session List

    private var sessionList: some View {
        List {
            // Filter chips
            filterChipsRow

            // Cross-device toggle
            allDevicesToggleRow

            // Search results or filtered sessions
            if let results = sessionStore.searchResults {
                searchResultsSection(results)
            } else if sessionStore.filteredSessions.isEmpty {
                emptyStateSection
            } else {
                ForEach(sessionStore.sessionSections) { section in
                    Section(section.title.uppercased()) {
                        ForEach(section.sessions) { session in
                            sessionRow(session, showPin: session.isPinned)
                        }
                    }
                }
            }

            // Load more
            if sessionStore.hasMore {
                loadMoreSection
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(Design.Colors.background)
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

    // MARK: - Session Row

    private func sessionRow(_ session: SessionSummary, showPin: Bool = false) -> some View {
        Button {
            close()
            Task { await sessionStore.switchToSession(session) }
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: session.sourceIcon)
                    .font(.system(size: Design.Size.iconSmall))
                    .foregroundStyle(
                        sessionStore.activeSessionID == session.id
                            ? Design.Brand.accent
                            : Design.Colors.secondaryForeground
                    )
                    .frame(width: 20)

                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 4) {
                        Text(session.title)
                            .font(Design.Typography.body)
                            .foregroundStyle(
                                sessionStore.activeSessionID == session.id
                                    ? Design.Brand.accent
                                    : Design.Colors.foreground
                            )
                            .lineLimit(1)
                        if showPin {
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

                Text(session.relativeTimeString)
                    .font(Design.Typography.caption2)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .listRowBackground(
            sessionStore.activeSessionID == session.id
                ? Design.Brand.accent.opacity(0.12)
                : Color.clear
        )
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
        .contextMenu {
            Button {
                Task { await sessionStore.togglePin(session) }
            } label: {
                Label(session.isPinned ? "Unpin" : "Pin",
                      systemImage: session.isPinned ? "pin.slash" : "pin")
            }
            Button {
                Task { await sessionStore.archiveSession(session) }
            } label: {
                Label("Archive", systemImage: "archivebox")
            }
            Button(role: .destructive) {
                Task { await sessionStore.deleteSession(session) }
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
    }

    // MARK: - Empty State

    private var emptyStateSection: some View {
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

    // MARK: - Load More

    private var loadMoreSection: some View {
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
            .padding(.vertical, Design.Spacing.sm)
        }
        .buttonStyle(.plain)
    }

    // MARK: - Actions

    private func open()   { setDrawer(open: true) }
    private func close()  { setDrawer(open: false) }
    private func snapOpen()   { withAnimation(Design.Motion.standard) { dragOffset = 0; isOpen = true } }
    private func snapClosed() { withAnimation(Design.Motion.standard) { dragOffset = 0; isOpen = false } }

    private func setDrawer(open: Bool) {
        withAnimation(Design.Motion.standard) {
            isOpen = open
            dragOffset = 0
        }
    }
}
