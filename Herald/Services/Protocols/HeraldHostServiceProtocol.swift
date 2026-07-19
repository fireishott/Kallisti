import Foundation

@MainActor
protocol HeraldHostServiceProtocol {
    func fetchCurrentHost(accessToken: String?) async throws -> HeraldHostStatus?
    func createEnrollmentCode(accessToken: String?) async throws -> HostEnrollmentCode
    func revokeCurrentHost(accessToken: String?) async throws
}
