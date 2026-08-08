import UIKit

// MARK: - Native Gateway (developer switch)

extension AppContainer {
    @MainActor
    static func mintTicketLoggingInIfNeeded(auth: NativeAuthCoordinator) async throws -> String {
        do {
            return try await auth.mintTicket()
        } catch NativeAuthError.notLoggedIn {
            guard let root = UIApplication.shared.connectedScenes
                .compactMap({ ($0 as? UIWindowScene)?.keyWindow })
                .first?.rootViewController else {
                throw NativeAuthError.notLoggedIn
            }
            try await auth.startLogin(presentingFrom: root)
            return try await auth.mintTicket()
        }
    }
}
