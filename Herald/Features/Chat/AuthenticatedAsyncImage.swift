import SwiftUI

/// An async image loader that injects the right bearer for internal URLs.
/// Drop-in replacement for `AsyncImage` when the image host requires
/// `Authorization: Bearer <token>` (relay-served, native, or DSH media).
/// The credential decision lives in `AttachmentService.authorizedRequest(for:)`
/// so this view and the fullscreen Save-to-Photos path cannot drift apart.
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
                    let req = await attachmentService.authorizedRequest(for: url)
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
