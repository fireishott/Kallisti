import AppIntents
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
        let client = RelayAPIClient(
            baseURLProvider: { HeraldAppState.shared.relayBaseURL }
        )

        struct StatusResponse: Decodable {
            struct Data: Decodable {
                let connected: Bool
                let activeJobs: Int
                let model: String?
                let version: String
                let uptimeSeconds: Int
            }
            let data: Data
        }

        do {
            let response: StatusResponse = try await client.get(
                path: "/gw/status",
                accessToken: HeraldAppState.shared.accessToken
            )
            await MainActor.run {
                GatewayState.shared.update(
                    connected: response.data.connected,
                    activeJobs: response.data.activeJobs,
                    model: response.data.model,
                    version: response.data.version
                )
            }
        } catch {
            await MainActor.run {
                GatewayState.shared.update(connected: false, activeJobs: 0, model: nil, version: nil)
            }
        }

        return .result()
    }
}
