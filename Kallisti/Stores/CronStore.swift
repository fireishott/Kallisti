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

    /// The daemon, when this phone is paired with one. The relay path is gone in
    /// this architecture, so without this the list is always empty.
    private(set) var herald: (any HeraldClientProtocol)?

    private let apiClient: RelayAPIClient?

    /// The daemon, found through the container when the store was built before it.
    private var resolvedHerald: (any HeraldClientProtocol)? {
        if let herald = resolvedHerald { return herald }
        return AppContainer.shared?.chatStore.heraldClient
    }

    /// Give the store the daemon once the container has built one.
    func attach(herald: any HeraldClientProtocol) {
        self.herald = herald
    }
    private let accessTokenProvider: () async -> String?

    init(
        apiClient: RelayAPIClient?,
        accessTokenProvider: @escaping () async -> String?,
        herald: (any HeraldClientProtocol)? = nil
    ) {
        self.herald = herald
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
    }

    func loadJobs() async {
        // A paired daemon is the source of truth. The relay path below stays for a
        // build that still carries one, but an Eris phone must never be told it is
        // "not connected to a relay" - there is no relay.
        if let herald {
            isLoading = true
            errorMessage = nil
            defer { isLoading = false }
            do {
                let rows = try await herald.cronJobs()
                jobs = rows.map { row in
                    CronJob(
                        id: row.id,
                        name: row.name,
                        schedule: row.schedule,
                        prompt: row.prompt,
                        enabled: row.enabled,
                        lastRun: row.lastRun.flatMap { ErisTimestamp.date(from: $0) },
                        nextRun: row.nextRun.flatMap { ErisTimestamp.date(from: $0) },
                        lastResult: row.lastStatus
                    )
                }
            } catch {
                // The daemon's own sentence: a phone paired without administration
                // is refused with an explanation, and that explanation is the
                // useful thing on screen.
                errorMessage = (error as? ErisError)?.message ?? error.localizedDescription
            }
            return
        }
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
