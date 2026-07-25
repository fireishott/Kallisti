import AppIntents
import SwiftUI
import WidgetKit

/// Control Center widget bundle for Herald gateway management (iOS 18+).
@main
struct HeraldControlsBundle: ControlWidgetBundle {
    var body: some ControlWidget {
        GatewayStatusControl()
        RestartGatewayControl()
        ModelSwitchControl()
    }
}
