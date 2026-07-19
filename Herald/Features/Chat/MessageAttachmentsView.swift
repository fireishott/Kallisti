import SwiftUI
import UIKit
import QuickLook

/// Renders the attachments on a chat message: images inline (tap to view full
/// screen) and files as cards (tap to open/share). Assistant images that ship
/// without a thumbnail are lazily fetched from the relay.
struct MessageAttachmentsView: View {
    let attachments: [MessageAttachment]
    /// User attachments hug the trailing edge; Hermes attachments the leading.
    var alignment: HorizontalAlignment = .leading

    @Environment(AttachmentService.self) private var attachmentService

    @State private var previewURL: IdentifiableURL?
    @State private var fullScreenImage: FullScreenImagePayload?

    private var images: [MessageAttachment] { attachments.filter(\.isImage) }
    private var files: [MessageAttachment] { attachments.filter { !$0.isImage } }

    var body: some View {
        VStack(alignment: alignment, spacing: Design.Spacing.xs) {
            if !images.isEmpty {
                imageLayout
            }
            ForEach(files) { file in
                AttachmentFileCard(attachment: file) {
                    Task { await openFile(file) }
                }
            }
        }
        .fullScreenCover(item: $fullScreenImage) { payload in
            FullScreenImageViewer(image: payload.image, fileName: payload.fileName)
        }
        .quickLookPreview($previewURL)
    }

    @ViewBuilder
    private var imageLayout: some View {
        if images.count == 1 {
            AttachmentImageView(attachment: images[0], maxWidth: 260, maxHeight: 320) { image in
                fullScreenImage = FullScreenImagePayload(image: image, fileName: images[0].fileName)
            }
        } else {
            let columns = [GridItem(.flexible(), spacing: Design.Spacing.xxs),
                           GridItem(.flexible(), spacing: Design.Spacing.xxs)]
            LazyVGrid(columns: columns, spacing: Design.Spacing.xxs) {
                ForEach(images) { image in
                    AttachmentImageView(attachment: image, maxWidth: 150, maxHeight: 150) { uiImage in
                        fullScreenImage = FullScreenImagePayload(image: uiImage, fileName: image.fileName)
                    }
                }
            }
            .frame(maxWidth: 320)
        }
    }

    private func openFile(_ attachment: MessageAttachment) async {
        guard let data = await attachmentService.data(for: attachment) else { return }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("HermesPreview", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appendingPathComponent(sanitized(attachment.fileName))
        do {
            try data.write(to: url, options: .atomic)
            previewURL = IdentifiableURL(url: url)
        } catch {
            // Silently ignore — the card just won't open.
        }
    }

    private func sanitized(_ name: String) -> String {
        let invalid = CharacterSet(charactersIn: "/:\\?%*|\"<>")
        let cleaned = name.components(separatedBy: invalid).joined(separator: "_")
        return cleaned.isEmpty ? "attachment" : cleaned
    }
}

private struct FullScreenImagePayload: Identifiable {
    let id = UUID()
    let image: UIImage
    let fileName: String
}

struct IdentifiableURL: Identifiable {
    let id = UUID()
    let url: URL
}

// MARK: - Inline image

/// Shows an attachment image: the persisted thumbnail first (instant), then the
/// full-resolution version once fetched. Tapping calls `onTap` with the loaded
/// image for full-screen presentation.
private struct AttachmentImageView: View {
    let attachment: MessageAttachment
    let maxWidth: CGFloat
    let maxHeight: CGFloat
    let onTap: (UIImage) -> Void

    @Environment(AttachmentService.self) private var attachmentService

    @State private var fullImage: UIImage?
    @State private var didStartLoad = false

    private var thumbnail: UIImage? {
        if let base64 = attachment.thumbnailBase64,
           let data = Data(base64Encoded: base64) {
            return UIImage(data: data)
        }
        if let path = attachment.localStoragePath,
           let data = try? Data(contentsOf: URL(fileURLWithPath: path)) {
            return UIImage(data: data)
        }
        return nil
    }

    var body: some View {
        Group {
            if let image = fullImage ?? thumbnail {
                Image(uiImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .frame(maxWidth: maxWidth, maxHeight: maxHeight)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
                    .overlay(
                        RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                            .stroke(Design.Colors.divider, lineWidth: 1)
                    )
                    .contentShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
                    .onTapGesture {
                        if let full = fullImage {
                            onTap(full)
                        } else {
                            Task { await loadAndOpen() }
                        }
                    }
            } else {
                placeholder
            }
        }
        .task {
            // Auto-load full image when there's no thumbnail (assistant images).
            guard !didStartLoad, thumbnail == nil else { return }
            didStartLoad = true
            fullImage = await attachmentService.image(for: attachment)
        }
    }

    private var placeholder: some View {
        RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
            .fill(Design.Colors.surface)
            .frame(width: min(maxWidth, 180), height: min(maxHeight, 140))
            .overlay(ProgressView())
    }

    private func loadAndOpen() async {
        let image = await attachmentService.image(for: attachment)
        if let image {
            fullImage = image
            onTap(image)
        }
    }
}

// MARK: - File card

private struct AttachmentFileCard: View {
    let attachment: MessageAttachment
    let onTap: () -> Void

    @State private var isLoading = false

    var body: some View {
        Button {
            isLoading = true
            onTap()
            // Reset shortly after — QuickLook presentation is driven by the parent.
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { isLoading = false }
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: iconName)
                    .font(.system(size: 20, weight: .regular))
                    .foregroundStyle(Design.Brand.accent)
                    .frame(width: 32, height: 32)
                    .background(Design.Colors.surface)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))

                VStack(alignment: .leading, spacing: 2) {
                    Text(attachment.fileName)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(typeLabel)
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }

                Spacer(minLength: 0)

                if isLoading {
                    ProgressView()
                } else {
                    Image(systemName: "arrow.up.right.square")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            }
            .padding(.horizontal, Design.Spacing.sm)
            .padding(.vertical, Design.Spacing.xs)
            .frame(maxWidth: 280, alignment: .leading)
            .background(Design.Colors.surface)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
            .overlay(
                RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                    .stroke(Design.Colors.divider, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }

    private var iconName: String {
        let mime = attachment.mimeType
        if mime.contains("pdf") { return "doc.richtext" }
        if mime.contains("json") || mime.contains("xml") || mime.contains("yaml") { return "curlybraces" }
        if mime.hasPrefix("text/") { return "doc.text" }
        if mime.contains("zip") || mime.contains("compressed") { return "doc.zipper" }
        return "doc"
    }

    private var typeLabel: String {
        let ext = (attachment.fileName as NSString).pathExtension.uppercased()
        return ext.isEmpty ? "File" : ext
    }
}

// MARK: - Full-screen image viewer

private struct FullScreenImageViewer: View {
    let image: UIImage
    let fileName: String

    @Environment(\.dismiss) private var dismiss
    @State private var scale: CGFloat = 1
    @State private var lastScale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var lastOffset: CGSize = .zero
    @State private var showShareSheet = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            Image(uiImage: image)
                .resizable()
                .aspectRatio(contentMode: .fit)
                .scaleEffect(scale)
                .offset(offset)
                .gesture(
                    MagnificationGesture()
                        .onChanged { value in scale = min(max(lastScale * value, 1), 5) }
                        .onEnded { _ in
                            lastScale = scale
                            if scale <= 1 {
                                withAnimation(.spring) { offset = .zero; lastOffset = .zero }
                            }
                        }
                )
                .simultaneousGesture(
                    DragGesture()
                        .onChanged { value in
                            guard scale > 1 else { return }
                            offset = CGSize(width: lastOffset.width + value.translation.width,
                                            height: lastOffset.height + value.translation.height)
                        }
                        .onEnded { _ in lastOffset = offset }
                )
                .onTapGesture(count: 2) {
                    withAnimation(.spring) {
                        if scale > 1 { scale = 1; lastScale = 1; offset = .zero; lastOffset = .zero }
                        else { scale = 2.5; lastScale = 2.5 }
                    }
                }

            VStack {
                HStack {
                    Button { dismiss() } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(.white)
                            .padding(10)
                            .background(.ultraThinMaterial, in: Circle())
                    }
                    Spacer()
                    Button { showShareSheet = true } label: {
                        Image(systemName: "square.and.arrow.up")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(.white)
                            .padding(10)
                            .background(.ultraThinMaterial, in: Circle())
                    }
                }
                .padding(Design.Spacing.md)
                Spacer()
            }
        }
        .statusBarHidden()
        .sheet(isPresented: $showShareSheet) {
            ShareSheet(items: [image])
        }
    }
}

// MARK: - UIKit bridges

struct ShareSheet: UIViewControllerRepresentable {
    let items: [Any]
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }
    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}

private extension View {
    /// Presents a QuickLook preview for the bound URL.
    func quickLookPreview(_ url: Binding<IdentifiableURL?>) -> some View {
        sheet(item: url) { identifiable in
            QuickLookPreview(url: identifiable.url)
                .ignoresSafeArea()
        }
    }
}

private struct QuickLookPreview: UIViewControllerRepresentable {
    let url: URL

    func makeCoordinator() -> Coordinator { Coordinator(url: url) }

    func makeUIViewController(context: Context) -> UINavigationController {
        let controller = QLPreviewController()
        controller.dataSource = context.coordinator
        return UINavigationController(rootViewController: controller)
    }

    func updateUIViewController(_ controller: UINavigationController, context: Context) {}

    final class Coordinator: NSObject, QLPreviewControllerDataSource {
        let url: URL
        init(url: URL) { self.url = url }
        func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }
        func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> QLPreviewItem {
            url as NSURL
        }
    }
}
