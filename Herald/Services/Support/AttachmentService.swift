import Foundation
import UIKit

/// Fetches and caches full-resolution attachment bytes from the relay.
///
/// Conversation loads only carry attachment metadata plus (for user uploads) a
/// small thumbnail. Assistant-produced images and any file body must be pulled
/// on demand from `messages/{id}/attachments/{index}`. Results are cached in
/// memory so scrolling and re-opening a viewer don't refetch.
@MainActor
@Observable
final class AttachmentService {
    @ObservationIgnored private let apiClient: RelayAPIClient?
    @ObservationIgnored private let accessTokenProvider: () async -> String?
    @ObservationIgnored private let accessTokenRefresher: () async -> String?

    @ObservationIgnored private let cache = NSCache<NSString, NSData>()
    @ObservationIgnored private var inflight: [String: Task<Data?, Never>] = [:]

    init(
        apiClient: RelayAPIClient?,
        accessTokenProvider: @escaping () async -> String?,
        accessTokenRefresher: @escaping () async -> String? = { nil }
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.accessTokenRefresher = accessTokenRefresher
        cache.totalCostLimit = 32 * 1024 * 1024  // ~32 MB of attachment bytes
    }

    /// Returns the raw bytes for an attachment, or nil if it can't be resolved.
    /// Local bytes (staged user uploads) are used directly; otherwise the relay
    /// endpoint is fetched and cached.
    func data(for attachment: MessageAttachment) async -> Data? {
        // Prefer a locally-staged copy (user's own uploads).
        if let path = attachment.localStoragePath,
           let localData = try? Data(contentsOf: URL(fileURLWithPath: path)) {
            return localData
        }

        guard let key = cacheKey(for: attachment) else { return nil }

        if let cached = cache.object(forKey: key as NSString) {
            return cached as Data
        }
        if let existing = inflight[key] {
            return await existing.value
        }

        guard let apiClient,
              let messageID = attachment.messageID,
              let index = attachment.remoteIndex else { return nil }

        let task = Task<Data?, Never> { [weak self] in
            guard let self else { return nil }
            let path = "messages/\(messageID.uuidString.lowercased())/attachments/\(index)"
            do {
                let token = await self.accessTokenProvider()
                do {
                    let (data, _) = try await apiClient.getRawData(path: path, accessToken: token)
                    return data
                } catch RelayAPIClient.ClientError.unauthorized {
                    guard let refreshed = await self.accessTokenRefresher(), !refreshed.isEmpty else { return nil }
                    let (data, _) = try await apiClient.getRawData(path: path, accessToken: refreshed)
                    return data
                }
            } catch {
                return nil
            }
        }
        inflight[key] = task
        let result = await task.value
        inflight[key] = nil
        if let result {
            cache.setObject(result as NSData, forKey: key as NSString, cost: result.count)
        }
        return result
    }

    /// Convenience: returns a decoded image for an image attachment.
    func image(for attachment: MessageAttachment) async -> UIImage? {
        guard let data = await data(for: attachment) else { return nil }
        return UIImage(data: data)
    }

    func accessToken() async -> String? { await accessTokenProvider() }

    private func cacheKey(for attachment: MessageAttachment) -> String? {
        guard let messageID = attachment.messageID, let index = attachment.remoteIndex else {
            return nil
        }
        return "\(messageID.uuidString)/\(index)"
    }
}
