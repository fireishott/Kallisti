import AppIntents
import KallistiSupport
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
        let client = GatewayControlClient(
            configuration: SharedRelayConfiguration.shared,
            credentials: SharedCredentialProvider.shared
        )

        do {
            let result = try await client.switchModel(name: model)
            if result.switched {
                await MainActor.run {
                    if let activeModel = result.model {
                        SharedGatewayStatus.shared.update(model: activeModel)
                    }
                }
                return .result(dialog: IntentDialog(stringLiteral: "Switched to \(result.model ?? model)"))
            } else {
                let reason = result.error ?? "unknown error"
                return .result(dialog: IntentDialog(stringLiteral: "Switch failed: \(reason)"))
            }
        } catch let error as GatewayControlError {
            return .result(dialog: IntentDialog(stringLiteral: error.errorDescription ?? "Switch failed."))
        } catch {
            return .result(dialog: IntentDialog(stringLiteral: "Switch failed: \(error.localizedDescription)"))
        }
    }
}
