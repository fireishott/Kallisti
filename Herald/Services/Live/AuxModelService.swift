import Foundation
import os

struct AuxTask: Decodable, Identifiable, Sendable {
    let key: String
    let task: String
    let provider: String
    let model: String
    let isAuto: Bool
    /// Stable row identifier - canonical config key (e.g. "browser_vision"),
    /// not the human label. Two rows can share the same label across
    /// reconnects, but the key is unique per task.
    var id: String { key }
}

private struct AuxListResponse: Decodable {
    let tasks: [AuxTask]
}

private struct AuxSetBody: Encodable {
    let task: String
    let provider: String
    let model: String
}

private struct AuxSetResponse: Decodable {
    let ok: Bool
}

@MainActor
@Observable
final class AuxModelService {
    private static let logger = Logger(subsystem: "net.fihonline.kallisti", category: "AuxModelService")
    private let apiClient: RelayAPIClient
    private let accessTokenProvider: @MainActor () async -> String?
    /// Native gateway feature client provider (nil when running legacy mode).
    private let nativeFeatureClientProvider: @MainActor () -> NativeGatewayFeatureClient?

    private(set) var tasks: [AuxTask] = []
    private(set) var lastError: String?

    init(
        apiClient: RelayAPIClient,
        accessTokenProvider: @escaping @MainActor () async -> String?,
        nativeFeatureClientProvider: @escaping @MainActor () -> NativeGatewayFeatureClient? = { nil }
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.nativeFeatureClientProvider = nativeFeatureClientProvider
    }

    func load() async {
        // NATIVE path: config.get(key:"full") -> config.auxiliary section.
        if let featureClient = nativeFeatureClientProvider() {
            do {
                let rows = try await featureClient.auxiliaryModels()
                tasks = rows.map { row in
                    AuxTask(
                        key: row.key,
                        task: row.label,
                        provider: row.provider,
                        model: row.model,
                        isAuto: !row.isOverride
                    )
                }
                lastError = nil
                return
            } catch {
                lastError = error.localizedDescription
                Self.logger.error("auxiliaryModels load failed: \(error.localizedDescription)")
                return
            }
        }

        do {
            let token = await accessTokenProvider()
            let response: AuxListResponse = try await apiClient.get(path: "aux", accessToken: token)
            tasks = response.tasks
            lastError = nil
        } catch {
            lastError = error.localizedDescription
            Self.logger.error("legacy aux GET failed: \(error.localizedDescription)")
        }
    }

    func set(task: String, provider: String, model: String) async {
        // Normalize every dashboard task label to its canonical config key
        // (auxiliary.<key>.<field>). The dashboard menu exposes these seven
        // tasks via the connector AUX_TASKS list; any new task the server
        // adds will fail normalization here and we surface a real error
        // instead of silently writing to a typo'd key.
        let normalizedTask: String
        switch task {
        case "Vision":          normalizedTask = "vision"
        case "Web extract":     normalizedTask = "web_extract"
        case "Compression":     normalizedTask = "compression"
        case "Session search":  normalizedTask = "session_search"
        case "Browser Vision":  normalizedTask = "browser_vision"
        case "MOA Reference":   normalizedTask = "moa_reference"
        case "MOA Aggregator":  normalizedTask = "moa_aggregator"
        default:
            // Allow callers that already pass the canonical key (snake_case).
            // Reject anything else with a real error instead of writing
            // garbage to the gateway config.
            let snake = task.lowercased().replacingOccurrences(of: " ", with: "_")
            if NativeGatewayFeatureClient.auxiliaryCatalog.contains(where: { $0.key == snake }) {
                normalizedTask = snake
            } else {
                lastError = "Unknown auxiliary task: \(task)"
                Self.logger.error("set rejected unknown task: \(task)")
                return
            }
        }

        // Prefer the native gateway cli.exec path. The relay /v1/aux endpoint
        // 401s on native bearer tokens in native mode; cli.exec is the same
        // wire the dashboard uses when it writes auxiliary.<task>.<key>.
        if let featureClient = nativeFeatureClientProvider() {
            do {
                // "auto" means "use server default"; the gateway's
                // config.set accepts an empty value for that case. Both
                // provider and model get mapped - leaving the model as the
                // literal string "auto" previously wrote "auto" into
                // auxiliary.vision.model and broke the override detection
                // in auxiliaryModels().
                let providerValue = (provider.lowercased() == "auto") ? "" : provider
                let modelValue    = (model.lowercased()    == "auto") ? "" : model
                Self.logger.info("aux set \(normalizedTask, privacy: .public) provider=\(providerValue, privacy: .public) model=\(modelValue, privacy: .public)")
                let providerArgv = ["config", "set", "auxiliary.\(normalizedTask).provider", providerValue]
                _ = try await featureClient.cliExec(argv: providerArgv, timeout: 30)
                let modelArgv = ["config", "set", "auxiliary.\(normalizedTask).model", modelValue]
                _ = try await featureClient.cliExec(argv: modelArgv, timeout: 30)
                await load()
                return
            } catch {
                // Surface the real gateway error. Do NOT fall back to the
                // legacy relay POST - that endpoint is dead in native mode
                // and the silent fallback masks the actual failure.
                lastError = error.localizedDescription
                Self.logger.error("cli.exec aux set failed for \(normalizedTask, privacy: .public): \(error.localizedDescription)")
                return
            }
        }

        // Legacy relay fallback - preserved for non-native mode. Surface
        // the real error rather than silently failing so the user can see
        // the relay-side failure in Settings > Infrastructure.
        do {
            let token = await accessTokenProvider()
            let _: AuxSetResponse = try await apiClient.post(
                path: "aux",
                body: AuxSetBody(task: normalizedTask, provider: provider, model: model),
                accessToken: token
            )
            await load()
        } catch {
            lastError = error.localizedDescription
            Self.logger.error("legacy aux POST failed for \(normalizedTask, privacy: .public): \(error.localizedDescription)")
        }
    }
}
