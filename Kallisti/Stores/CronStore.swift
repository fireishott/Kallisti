import Foundation

/// Manages cron job CRUD via the relay's `/v1/cron` endpoints.
@MainActor
@Observable
final class CronStore {
    struct CronJob: Decodable, Identifiable, Hashable {
        let id: String
        let name: String
        let schedule: String
        let prompt: String
        var enabled: Bool
        let lastRun: Date?
        let nextRun: Date?
        let lastResult: String?
    }

    private struct CronListResponse: Decodable {
        let jobs: [CronJob]
    }

    private struct CronJobResponse: Decodable {
        let job: CronJob
    }

    private struct CronCreateBody: Encodable {
        let name: String
        let schedule: String
        let prompt: String
    }

    private struct CronUpdateBody: Encodable {
        let enabled: Bool
    }

    private struct EmptyResponse: Decodable {}

    private(set) var jobs: [CronJob] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    private let apiClient: RelayAPIClient?
    private let accessTokenProvider: () async -> String?

    init(apiClient: RelayAPIClient?, accessTokenProvider: @escaping () async -> String?) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
    }

    func loadJobs() async {
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        do {
            let response: CronListResponse = try await apiClient.get(
                path: "cron", accessToken: token
            )
            jobs = response.jobs
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func createJob(name: String, schedule: String, prompt: String) async throws {
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }
        let body = CronCreateBody(name: name, schedule: schedule, prompt: prompt)
        let response: CronJobResponse = try await apiClient.post(
            path: "cron", body: body, accessToken: token
        )
        jobs.append(response.job)
    }

    func toggleJob(_ job: CronJob) async throws {
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }
        let body = CronUpdateBody(enabled: !job.enabled)
        let response: CronJobResponse = try await apiClient.patch(
            path: "cron/\(job.id)", body: body, accessToken: token
        )
        if let index = jobs.firstIndex(where: { $0.id == job.id }) {
            jobs[index] = response.job
        }
    }

    func deleteJob(_ job: CronJob) async throws {
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }
        let _: EmptyResponse = try await apiClient.delete(
            path: "cron/\(job.id)", accessToken: token
        )
        jobs.removeAll { $0.id == job.id }
    }

    func reset() {
        jobs = []
        errorMessage = nil
    }
}
