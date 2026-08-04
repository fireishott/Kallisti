import AppIntents
import KallistiSupport
import SwiftUI
import WidgetKit

/// Control Center widget to restart the Herald gateway or the connector.
///
/// The restart is destructive — Build 108 §15A.6 requires an explicit
/// confirmation before POST.  Apple's `ControlWidget` API does not expose
/// a native confirmation dialog, so this widget launches the Herald app
/// onto its confirmed restart screen instead of executing the restart
/// directly.  When iOS eventually ships a Control Center confirmation
/// surface, the deep-link fallback can be removed.
struct RestartGatewayControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(
            kind: "com.fireishott.Herald.restart-gateway"
        ) {
            ControlWidgetButton(action: RestartGatewayIntent()) {
                Label("Restart GW", systemImage: "arrow.triangle.2.circlepath")
            }
            .tint(.orange)
        }
        .displayName("Restart Gateway")
        .description("Restart the Herald gateway or connector — opens Herald to confirm.")
    }
}

// MARK: - Restart Intent

struct RestartGatewayIntent: AppIntent {
    static let title: LocalizedStringResource = "Restart Gateway"

    /// Open in app when run from Control Center so the user can confirm.
    static let openAppWhenRun: Bool = true

    @Parameter(title: "Target", default: "hermes")
    var target: String

    init() {}

    init(target: String) {
        self.target = target
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        // Build 108 §15A.6: the Control Widget API does not provide a
        // confirmation dialog sufficient for a destructive action.  Deep-
        // link into the main app's confirmed restart screen rather than
        // executing the restart directly from Control Center.
        // The actual POST /gw/restart with Idempotency-Key and the polling
        // are performed by `GatewayControlService` in the host app, which
        // surfaces the typed `GatewayControlError` cases through its own
        // confirmation UI.
        return .result()
    }
}
