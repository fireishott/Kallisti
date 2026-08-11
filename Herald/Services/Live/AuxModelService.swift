import Foundation
import os

struct AuxTask: Decodable, Identifiable, Sendable {
    let task: String
    let provider: String
    let model: String
    let isAuto: Bool
    var id: String { task }
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
                        task: row.label,
                        provider: row.provider,
                        model: row.model,
                        isAuto: !row.isOverride
                    )
                }
                lastError = nil
                return
            } catch {
                lastError = nil
                // Fall through to legacy path.
            }
        }

        do {
            let token = await accessTokenProvider()
            let response: AuxListResponse = try await apiClient.get(path: "aux", accessToken: token)
            tasks = response.tasks
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    func set(task: String, provider: String, model: String) async {
        let normalizedTask: String
        switch task {
        case "Vision": normalizedTask = "vision"
        case "Web extract": normalizedTask = "web_extract"
        case "Compression": normalizedTask = "compression"
        default: normalizedTask = task.lowercased()
        }

        // Prefer the native gateway cli.exec path - the relay /v1/aux endpoint
        // is dead in native mode (relay REST 401s on native bearer tokens).
        if let featureClient = nativeFeatureClientProvider() {
            do {
                // "auto" means "use server default"; the gateway's config.set
                // accepts an empty value for that case.
                let providerValue = (provider.lowercased() == "auto") ? "" : provider
                let providerArgv = ["config", "set", "auxiliary.\(normalizedTask).provider", providerValue]
                _ = try await featureClient.cliExec(argv: providerArgv, timeout: 30)
                let modelArgv = ["config", "set", "auxiliary.\(normalizedTask).model", model]
                _ = try await featureClient.cliExec(argv: modelArgv, timeout: 30)
                await load()
                return
            } catch {
                lastError = error.localizedDescription
                return
            }
        }

        // Legacy relay fallback - preserved for non-native mode.
        do {
            let token = await accessTokenProvider()
            let _: AuxSetResponse = try await apiClient.post(
                path: "aux",
                body: AuxSetBody(task: task, provider: provider, model: model),
                accessToken: token
            )
            await load()
        } catch {
            lastError = error.localizedDescription
        }
    }
}
