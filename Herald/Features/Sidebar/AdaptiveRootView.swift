import SwiftUI

/// Root view that adapts to device class:
/// - iPad: Two-column NavigationSplitView (sidebar + content) with optional trailing inspector
/// - iPhone (both orientations): TabView with slide-out session drawer
struct AdaptiveRootView: View {
    @Environment(TabRouter.self) private var router
    @State private var selectedSection: SidebarSection = .chat
    @State private var isRightPanelOpen = false
    @State private var rightPanelTab: RightPanelTab = .logs
    @State private var rightPanelWidth: CGFloat = 320

    private let minInspectorWidth: CGFloat = 280
    private let maxInspectorWidth: CGFloat = 480
    private let minChatWidth: CGFloat = 420

    private var useIPadLayout: Bool {
        DeviceClass.isPad
    }

    var body: some View {
        if useIPadLayout {
            iPadLayout
                .onAppear { installRouterBinding() }
                .onDisappear { removeRouterBinding() }
        } else {
            MainTabView()
        }
    }

    // MARK: - iPad Layout (Two-column split + optional inspector)

    private var iPadLayout: some View {
        GeometryReader { geometry in
            let availableWidth = geometry.size.width
            let clampedInspectorWidth = clampInspectorWidth(
                available: availableWidth
            )

            HStack(spacing: 0) {
                // Main split: sidebar + content (detail is hidden/unused)
                NavigationSplitView {
                    iPadSidebarView(
                        selectedSection: $selectedSection,
                        isRightPanelOpen: $isRightPanelOpen
                    )
                } content: {
                    contentColumn
                } detail: {
                    // Empty detail — the real inspector is a sibling HStack element,
                    // not inside the split view. This keeps the split view two-column
                    // in practice while satisfying iOS's three-column requirement.
                    Color.clear
                }
                .navigationSplitViewColumnWidth(
                    min: 280, ideal: 360, max: 400
                )
                .gesture(
                    DragGesture(minimumDistance: 20)
                        .onEnded { value in
                            if value.translation.width < -50 && !isRightPanelOpen {
                                withAnimation(Design.Motion.standard) {
                                    isRightPanelOpen = true
                                }
                            } else if value.translation.width > 50 && isRightPanelOpen {
                                withAnimation(Design.Motion.standard) {
                                    isRightPanelOpen = false
                                }
                            }
                        }
                )

                // Inspector: genuinely inserted/removed from layout
                if isRightPanelOpen {
                    inspectorDivider(clampedWidth: clampedInspectorWidth)

                    iPadRightPanelView(
                        isOpen: $isRightPanelOpen,
                        selectedTab: $rightPanelTab
                    )
                    .frame(width: clampedInspectorWidth)
                    .background(Design.Colors.surface)
                    .transition(.move(edge: .trailing).combined(with: .opacity))
                }
            }
            .animation(Design.Motion.standard, value: isRightPanelOpen)
        }
    }

    /// Clamp inspector width so chat never drops below the readable minimum.
    private func clampInspectorWidth(available: CGFloat) -> CGFloat {
        let sidebarBudget: CGFloat = 360
        let maxAllowed = available - sidebarBudget - minChatWidth
        let clamped = min(rightPanelWidth, max(maxAllowed, minInspectorWidth))
        return min(clamped, maxInspectorWidth)
    }

    /// Drag handle between content and inspector for resizing.
    private func inspectorDivider(clampedWidth: CGFloat) -> some View {
        Rectangle()
            .fill(Design.Colors.divider)
            .frame(width: 1)
            .contentShape(Rectangle())
            .frame(width: 8)
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { value in
                        let newWidth = clampedWidth - value.translation.width
                        rightPanelWidth = max(
                            minInspectorWidth,
                            min(newWidth, maxInspectorWidth)
                        )
                    }
            )
            .resizeCursor()
    }

    @ViewBuilder
    private var contentColumn: some View {
        switch selectedSection {
        case .chat:
            ChatScreen(isSessionDrawerOpen: .constant(false))
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        rightPanelToggle
                    }
                }
        case .inbox:
            InboxScreen()
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        rightPanelToggle
                    }
                }
        case .talk:
            TalkModeScreen()
        case .settings:
            SettingsScreen()
        }
    }

    private var rightPanelToggle: some View {
        Button {
            withAnimation(Design.Motion.standard) {
                isRightPanelOpen.toggle()
            }
        } label: {
            Image(systemName: "sidebar.right")
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(
                    isRightPanelOpen ? Design.Brand.accent : Design.Colors.secondaryForeground
                )
        }
        .buttonStyle(.plain)
        .accessibilityLabel(isRightPanelOpen ? "Close inspector" : "Open inspector")
    }

    // MARK: - Router Synchronization

    private func installRouterBinding() {
        router.oniPadSectionSwitch = { [self] section in
            withAnimation {
                selectedSection = section
            }
        }
        syncRouterToSection()
    }

    private func removeRouterBinding() {
        router.oniPadSectionSwitch = nil
    }

    private func syncRouterToSection() {
        switch router.selectedTab {
        case .chat: selectedSection = .chat
        case .inbox: selectedSection = .inbox
        case .talk: selectedSection = .talk
        case .settings: selectedSection = .settings
        }
    }
}

// MARK: - Cursor helper (no-op on iOS/iPadOS — drag handle still works via gesture)

#if canImport(AppKit)
import AppKit

extension View {
    func resizeCursor() -> some View {
        self.onHover { inside in
            if inside { NSCursor.resizeLeftRight.push() } else { NSCursor.pop() }
        }
    }
}
#else
extension View {
    func resizeCursor() -> some View { self }
}
#endif
