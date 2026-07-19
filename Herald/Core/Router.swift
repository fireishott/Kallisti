import SwiftUI

// MARK: - Navigation Routes

enum Route: Hashable {
    case permissions
    case capture
    case connectHost
}

// MARK: - Sheet Destinations

enum SheetDestination: Identifiable {
    case settings
    case attachments
    case newChat

    var id: String {
        switch self {
        case .settings: "settings"
        case .attachments: "attachments"
        case .newChat: "newChat"
        }
    }
}

// MARK: - App Tab (kept for backward compatibility during transition)

enum AppTab: String, CaseIterable, Identifiable {
    case chat
    case inbox
    case talk
    case settings

    var id: String { rawValue }

    var title: String {
        switch self {
        case .chat: "Chat"
        case .inbox: "Inbox"
        case .talk: "Talk"
        case .settings: "Settings"
        }
    }

    var icon: String {
        switch self {
        case .chat: "bubble.left.and.bubble.right"
        case .inbox: "tray"
        case .talk: "waveform"
        case .settings: "gearshape"
        }
    }
}

// MARK: - Router

@MainActor
@Observable
final class TabRouter {
    var selectedTab: AppTab = .chat
    var activeSheet: SheetDestination?
    var isVoiceOverlayPresented = false
    private var navigationPath: [Route] = []

    // MARK: iPad section binding
    /// When set (by AdaptiveRootView on iPad), the router calls this instead of
    /// presenting a sheet for sections that have a dedicated sidebar destination.
    var oniPadSectionSwitch: ((SidebarSection) -> Void)?

    func path() -> [Route] {
        navigationPath
    }

    func binding(for tab: AppTab) -> Binding<[Route]> {
        Binding(
            get: { self.navigationPath },
            set: { self.navigationPath = $0 }
        )
    }

    func pathBinding() -> Binding<[Route]> {
        Binding(
            get: { self.navigationPath },
            set: { self.navigationPath = $0 }
        )
    }

    func navigate(to route: Route, in tab: AppTab? = nil) {
        navigationPath.append(route)
    }

    func popToRoot(for tab: AppTab? = nil) {
        navigationPath = []
    }

    func resetAll() {
        navigationPath.removeAll()
    }

    func presentSheet(_ sheet: SheetDestination) {
        // On iPad, settings is shown via sidebar section switch, not as a sheet.
        // Other sheets (attachments, newChat) and iPhone fallback use the sheet.
        if sheet == .settings, let switcher = oniPadSectionSwitch {
            switcher(.settings)
            return
        }
        activeSheet = sheet
    }

    func dismissSheet() {
        activeSheet = nil
    }
}
