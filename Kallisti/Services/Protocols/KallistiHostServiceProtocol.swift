import Foundation

@MainActor
protocol KallistiHostServiceProtocol {
    func fetchCurrentHost(accessToken: String?) async throws -> KallistiHostStatus?
    func createEnrollmentCode(accessToken: String?) async throws -> HostEnrollmentCode
    func revokeCurrentHost(accessToken: String?) async throws
}
