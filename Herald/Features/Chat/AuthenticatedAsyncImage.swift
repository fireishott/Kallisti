import SwiftUI

/// An async image loader that injects the relay auth token for internal URLs.
/// Drop-in replacement for `AsyncImage` when the image host requires
/// `Authorization: Bearer <token>` (e.g. relay-served images).
struct AuthenticatedAsyncImage<Content: View>: View {
    let url: URL
    @ViewBuilder let content: (AsyncImagePhase) -> Content
    @Environment(AttachmentService.self) private var attachmentService

    @State private var phase: AsyncImagePhase = .empty

    var body: some View {
        content(phase)
            .task(id: url) {
                phase = .empty
                do {
                    var req = URLRequest(url: url)
                    // Only attach auth for internal relay hosts
                    if url.host?.contains("192.168") == true
                        || url.host?.contains("fihonline") == true
                        || url.host?.contains("hermes-relay") == true {
                        if let token = await attachmentService.accessToken() {
                            req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                        }
                    }
                    let (data, response) = try await URLSession.shared.data(for: req)
                    guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                        phase = .failure(URLError(.badServerResponse))
                        return
                    }
                    guard let uiImage = UIImage(data: data) else {
                        phase = .failure(URLError(.cannotDecodeContentData))
                        return
                    }
                    phase = .success(Image(uiImage: uiImage))
                } catch {
                    phase = .failure(error)
                }
            }
    }
}
