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
    private let nativeFeatureClientProvider: () -> NativeGatewayFeatureClient?

    init(apiClient: RelayAPIClient?, accessTokenProvider: @escaping () async -> String?, nativeFeatureClientProvider: @escaping () -> NativeGatewayFeatureClient? = { nil }) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.nativeFeatureClientProvider = nativeFeatureClientProvider
    }

    func loadJobs() async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        do {
            if let nativeFeatureClient = nativeFeatureClientProvider() {
                jobs = try await nativeFeatureClient.managedCronJobs().map { CronJob(id: $0.id, name: $0.name, schedule: $0.schedule, prompt: $0.prompt, enabled: $0.enabled, lastRun: $0.lastRun, nextRun: $0.nextRun, lastResult: $0.lastResult) }
            } else {
                guard let apiClient, let token = await accessTokenProvider() else {
                    // Build 135.40: keep the loaded list on connectivity miss.
                    errorMessage = "Not connected to a relay."
                    return
                }
                let response: CronListResponse = try await apiClient.get(path: "cron", accessToken: token)
                jobs = response.jobs
            }
        } catch {
            // Build 135.40: failed refresh keeps the existing list so back-nav
            // never blanks a populated cron list to the error/empty state.
            if jobs.isEmpty {
                errorMessage = error.localizedDescription
            }
        }
    }

    func createJob(name: String, schedule: String, prompt: String) async throws {
        if let nativeFeatureClient = nativeFeatureClientProvider() {
            let job = try await nativeFeatureClient.createManagedCronJob(name: name, schedule: schedule, prompt: prompt)
            jobs.append(CronJob(id: job.id, name: job.name, schedule: job.schedule, prompt: job.prompt, enabled: job.enabled, lastRun: job.lastRun, nextRun: job.nextRun, lastResult: job.lastResult))
            return
        }
        guard let apiClient, let token = await accessTokenProvider() else { errorMessage = "Not connected to a relay."; return }
        let response: CronJobResponse = try await apiClient.post(path: "cron", body: CronCreateBody(name: name, schedule: schedule, prompt: prompt), accessToken: token)
        jobs.append(response.job)
    }

    func toggleJob(_ job: CronJob) async throws {
        if let nativeFeatureClient = nativeFeatureClientProvider() {
            let updated = try await nativeFeatureClient.updateManagedCronJob(id: job.id, enabled: !job.enabled)
            if let index = jobs.firstIndex(where: { $0.id == job.id }) { jobs[index] = CronJob(id: updated.id, name: updated.name, schedule: updated.schedule, prompt: updated.prompt, enabled: updated.enabled, lastRun: updated.lastRun, nextRun: updated.nextRun, lastResult: updated.lastResult) }
            return
        }
        guard let apiClient, let token = await accessTokenProvider() else { errorMessage = "Not connected to a relay."; return }
        let response: CronJobResponse = try await apiClient.patch(path: "cron/\(job.id)", body: CronUpdateBody(enabled: !job.enabled), accessToken: token)
        if let index = jobs.firstIndex(where: { $0.id == job.id }) { jobs[index] = response.job }
    }

    /// Build 135.40: persist name/schedule/prompt edits from the rich editor.
    func updateJobContent(id: String, name: String, schedule: String, prompt: String) async throws {
        if let nativeFeatureClient = nativeFeatureClientProvider() {
            let updated = try await nativeFeatureClient.updateManagedCronJobContent(id: id, name: name, schedule: schedule, prompt: prompt)
            if let index = jobs.firstIndex(where: { $0.id == id }) {
                jobs[index] = CronJob(id: updated.id, name: updated.name, schedule: updated.schedule, prompt: updated.prompt, enabled: jobs[index].enabled, lastRun: jobs[index].lastRun, nextRun: jobs[index].nextRun, lastResult: jobs[index].lastResult)
            }
            return
        }
        guard let apiClient, let token = await accessTokenProvider() else { errorMessage = "Not connected to a relay."; return }
        struct CronUpdateContentBody: Encodable { let name: String; let schedule: String; let prompt: String }
        let response: CronJobResponse = try await apiClient.patch(path: "cron/\(id)", body: CronUpdateContentBody(name: name, schedule: schedule, prompt: prompt), accessToken: token)
        if let index = jobs.firstIndex(where: { $0.id == id }) { jobs[index] = response.job }
    }

    func deleteJob(_ job: CronJob) async throws {
        if let nativeFeatureClient = nativeFeatureClientProvider() {
            try await nativeFeatureClient.deleteManagedCronJob(id: job.id)
            jobs.removeAll { $0.id == job.id }
            return
        }
        guard let apiClient, let token = await accessTokenProvider() else { errorMessage = "Not connected to a relay."; return }
        let _: EmptyResponse = try await apiClient.delete(path: "cron/\(job.id)", accessToken: token)
        jobs.removeAll { $0.id == job.id }
    }

    func reset() {
        jobs = []
        errorMessage = nil
    }
}
