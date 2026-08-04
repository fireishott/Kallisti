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

    private(set) var tasks: [AuxTask] = []
    private(set) var lastError: String?

    init(apiClient: RelayAPIClient, accessTokenProvider: @escaping @MainActor () async -> String?) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
    }

    func load() async {
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
