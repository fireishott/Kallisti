import AppIntents
import SwiftUI
import WidgetKit

/// Control Center widget to restart the Herald gateway, connector, or Hermes agent.
/// Long-press to choose the target, tap to confirm.
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
        .description("Restart the Herald gateway, connector, or Hermes agent.")
    }
}

// MARK: - Restart Intent

struct RestartGatewayIntent: AppIntent {
    static let title: LocalizedStringResource = "Restart Gateway"

    @Parameter(title: "Target", default: "relay")
    var target: String

    init() {}

    init(target: String) {
        self.target = target
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        let client = RelayAPIClient(
            baseURLProvider: { HeraldAppState.shared.relayBaseURL }
        )

        struct RestartRequest: Encodable {
            let target: String
        }

        struct RestartResponse: Decodable {
            let restarting: Bool
            let target: String
            let message: String?
        }

        let body = RestartRequest(target: target)
        let response: RestartResponse = try await client.postGateway(
            path: "/gw/restart",
            body: body,
            accessToken: HeraldAppState.shared.accessToken
        )

        if response.restarting {
            return .result(dialog: "\(response.target) restart initiated")
        } else {
            return .result(dialog: "Restart failed: \(response.message ?? "unknown error")")
        }
    }
}
