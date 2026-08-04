import AppIntents
import HeraldSupport
import SwiftUI
import WidgetKit

/// Control Center widget showing Herald gateway connection status.
/// Tap to refresh — the intent queries /gw/status and updates the display.
struct GatewayStatusControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(
            kind: "com.fireishott.Herald.gateway-status"
        ) {
            ControlWidgetButton(action: RefreshGatewayStatusIntent()) {
                Label {
                    Text("Herald GW")
                } icon: {
                    Image(systemName: "network")
                }
            }
            .tint(.blue)
        }
        .displayName("Gateway Status")
        .description("Shows Herald gateway connection status. Tap to refresh.")
    }
}

// MARK: - Refresh Intent

struct RefreshGatewayStatusIntent: AppIntent {
    static let title: LocalizedStringResource = "Refresh Gateway Status"

    @MainActor
    func perform() async throws -> some IntentResult {
        let client = GatewayControlClient(
            configuration: SharedRelayConfiguration.shared,
            credentials: SharedCredentialProvider.shared
        )

        do {
            let status = try await client.fetchStatus()
            await MainActor.run {
                SharedGatewayStatus.shared.update(
                    connected: status.connectorConnected && (status.hermesActive ?? true),
                    activeJobs: status.activeJobs,
                    model: status.activeModel,
                    version: status.connectorVersion
                )
            }
        } catch {
            await MainActor.run {
                SharedGatewayStatus.shared.update(
                    connected: false,
                    activeJobs: 0,
                    model: nil,
                    version: nil
                )
            }
        }

        return .result()
    }
}
