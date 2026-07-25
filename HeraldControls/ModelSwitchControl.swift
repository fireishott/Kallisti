import AppIntents
import SwiftUI
import WidgetKit

/// Control Center widget for quick model switching on the Herald gateway.
struct ModelSwitchControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(
            kind: "com.fireishott.Herald.model-switch"
        ) {
            ControlWidgetButton(action: ModelSwitchIntent()) {
                Label("Switch Model", systemImage: "brain.head.profile")
            }
            .tint(.purple)
        }
        .displayName("Switch Model")
        .description("Quickly switch the active AI model on the gateway.")
    }
}

// MARK: - Model Switch Intent

struct ModelSwitchIntent: AppIntent {
    static let title: LocalizedStringResource = "Switch Model"

    @Parameter(title: "Model")
    var model: String

    init() {}

    init(model: String) {
        self.model = model
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        let client = RelayAPIClient(
            baseURLProvider: { HeraldAppState.shared.relayBaseURL }
        )

        struct SwitchRequest: Encodable {
            let model: String
        }

        struct SwitchResponse: Decodable {
            struct Data: Decodable {
                let switched: Bool
                let model: String?
                let error: String?
            }
            let data: Data
        }

        let body = SwitchRequest(model: model)
        let response: SwitchResponse = try await client.post(
            path: "/gw/model/switch",
            body: body,
            accessToken: HeraldAppState.shared.accessToken
        )

        if response.data.switched {
            await MainActor.run {
                GatewayState.shared.update(model: response.data.model)
            }
            return .result(dialog: "Switched to \(response.data.model ?? model)")
        } else {
            return .result(dialog: "Failed: \(response.data.error ?? "unknown")")
        }
    }
}
