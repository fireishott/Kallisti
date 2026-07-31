import SwiftUI
import UIKit
import QuickLook
import UniformTypeIdentifiers

/// Renders the attachments on a chat message: images inline (tap to view full
/// screen) and files as cards (tap to open/share). Assistant images that ship
/// without a thumbnail are lazily fetched from the relay.
struct MessageAttachmentsView: View {
    let attachments: [MessageAttachment]
    /// User attachments hug the trailing edge; Herald attachments the leading.
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
            FullScreenImageViewer(
                image: payload.image,
                fileName: payload.fileName,
                imageData: payload.imageData,
                mimeType: payload.mimeType
            )
        }
        .quickLookPreview($previewURL)
    }

    @ViewBuilder
    private var imageLayout: some View {
        if images.count == 1 {
            AttachmentImageView(attachment: images[0], maxWidth: 260, maxHeight: 320) { uiImage, data, mime in
                fullScreenImage = FullScreenImagePayload(
                    image: uiImage, fileName: images[0].fileName,
                    imageData: data, mimeType: mime
                )
            }
        } else {
            let columns = [GridItem(.flexible(), spacing: Design.Spacing.xxs),
                           GridItem(.flexible(), spacing: Design.Spacing.xxs)]
            LazyVGrid(columns: columns, spacing: Design.Spacing.xxs) {
                ForEach(images) { image in
                    AttachmentImageView(attachment: image, maxWidth: 150, maxHeight: 150) { uiImage, data, mime in
                        fullScreenImage = FullScreenImagePayload(
                            image: uiImage, fileName: image.fileName,
                            imageData: data, mimeType: mime
                        )
                    }
                }
            }
            .frame(maxWidth: 320)
        }
    }

    private func openFile(_ attachment: MessageAttachment) async {
        guard let data = await attachmentService.data(for: attachment) else { return }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("HeraldPreview", isDirectory: true)
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
    let imageData: Data?
    let mimeType: String?
}

struct IdentifiableURL: Identifiable {
    let id = UUID()
    let url: URL
}

// MARK: - Inline image

/// Shows an attachment image: the persisted thumbnail first (instant), then the
/// full-resolution version once fetched. Tapping opens the full-screen viewer.
/// Uses a real Button (not just onTapGesture) for accessibility, and shows
/// retry/error UI when the full-image fetch fails instead of an indefinite spinner.
private struct AttachmentImageView: View {
    let attachment: MessageAttachment
    let maxWidth: CGFloat
    let maxHeight: CGFloat
    let onTap: (UIImage, Data?, String?) -> Void

    @Environment(AttachmentService.self) private var attachmentService

    @State private var fullImage: UIImage?
    @State private var fullImageData: Data?
    @State private var didStartLoad = false
    @State private var loadFailed = false

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
                Button {
                    if let full = fullImage {
                        onTap(full, fullImageData, attachment.mimeType)
                    } else {
                        Task { await loadAndOpen() }
                    }
                } label: {
                    Image(uiImage: image)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                        .frame(maxWidth: maxWidth, maxHeight: maxHeight)
                        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
                        .overlay(
                            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                                .stroke(Design.Colors.divider, lineWidth: 1)
                        )
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Image: \(attachment.fileName)")
                .accessibilityHint("Double-tap to view full screen")
            } else if loadFailed {
                retryPlaceholder
            } else {
                placeholder
            }
        }
        .task {
            // Auto-load full image when there's no thumbnail (assistant images).
            guard !didStartLoad, thumbnail == nil else { return }
            didStartLoad = true
            await loadFullImage()
        }
    }

    private var placeholder: some View {
        RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
            .fill(Design.Colors.surface)
            .frame(width: min(maxWidth, 180), height: min(maxHeight, 140))
            .overlay(ProgressView())
    }

    private var retryPlaceholder: some View {
        Button {
            loadFailed = false
            didStartLoad = false
            Task { await loadFullImage() }
        } label: {
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .fill(Design.Colors.surface)
                .frame(width: min(maxWidth, 180), height: min(maxHeight, 140))
                .overlay(
                    VStack(spacing: 4) {
                        Image(systemName: "arrow.clockwise")
                            .font(.system(size: 18, weight: .medium))
                        Text("Tap to retry")
                            .font(Design.Typography.caption2)
                    }
                    .foregroundStyle(Design.Colors.secondaryForeground)
                )
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Image load failed. \(attachment.fileName)")
        .accessibilityHint("Double-tap to retry loading")
    }

    private func loadFullImage() async {
        loadFailed = false
        if let data = await attachmentService.data(for: attachment),
           let image = UIImage(data: data) {
            fullImage = image
            fullImageData = data
        } else {
            loadFailed = true
        }
    }

    private func loadAndOpen() async {
        await loadFullImage()
        if let image = fullImage {
            onTap(image, fullImageData, attachment.mimeType)
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
    /// Full-resolution bytes, if available.  When set, share and download
    /// use a temporary file (preserving filename/MIME) rather than a bare
    /// UIImage — the share sheet then offers all native destinations and
    /// the file exporter writes to a user-chosen Files location.
    let imageData: Data?
    let mimeType: String?

    @Environment(\.dismiss) private var dismiss
    @State private var scale: CGFloat = 1
    @State private var lastScale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var lastOffset: CGSize = .zero
    @State private var showShareSheet = false
    @State private var showFileExporter = false
    @State private var tempFileURL: URL?
    @State private var saveError: String?

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
                    .accessibilityLabel("Close viewer")
                    Spacer()
                    Button {
                        prepareTempFile()
                        if tempFileURL != nil { showFileExporter = true }
                        else { saveToPhotos() }
                    } label: {
                        Image(systemName: "square.and.arrow.down")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(.white)
                            .padding(10)
                            .background(.ultraThinMaterial, in: Circle())
                    }
                    .accessibilityLabel("Save image")
                    Button {
                        prepareTempFile()
                        showShareSheet = true
                    } label: {
                        Image(systemName: "square.and.arrow.up")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(.white)
                            .padding(10)
                            .background(.ultraThinMaterial, in: Circle())
                    }
                    .accessibilityLabel("Share image")
                }
                .padding(Design.Spacing.md)

                if let saveError {
                    Text(saveError)
                        .font(Design.Typography.caption2)
                        .foregroundStyle(.white)
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.vertical, 6)
                        .background(.red.opacity(0.7), in: RoundedRectangle(cornerRadius: 8))
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
                Spacer()
            }
        }
        .statusBarHidden()
        .sheet(isPresented: $showShareSheet) {
            if let url = tempFileURL {
                ShareSheet(items: [url])
            } else {
                ShareSheet(items: [image])
            }
        }
        .fileExporter(
            isPresented: $showFileExporter,
            document: tempFileURL.flatMap { TempFileDocument(url: $0) },
            contentType: utTypeForMime(),
            defaultFilename: fileName
        ) { result in
            if case .failure(let error) = result {
                withAnimation { saveError = error.localizedDescription }
            }
        }
        .onDisappear { cleanupTempFile() }
    }

    /// Materialize the image bytes to a temporary file so share/download
    /// surfaces preserve the original filename and MIME type.  A bare
    /// UIImage in the share sheet strips both.
    private func prepareTempFile() {
        guard tempFileURL == nil else { return }
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("HeraldImageShare", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let ext = (fileName as NSString).pathExtension
        let base = (fileName as NSString).deletingPathExtension
        let safe = sanitized(base.isEmpty ? "image" : base)
        let url = dir.appendingPathComponent("\(safe).\(ext.isEmpty ? "png" : ext)")
        let data = imageData ?? image.pngData() ?? image.jpegData(compressionQuality: 0.92)
        do {
            try data?.write(to: url, options: .atomic)
            tempFileURL = url
        } catch {
            saveError = "Could not prepare image for sharing."
        }
    }

    private func cleanupTempFile() {
        if let url = tempFileURL { try? FileManager.default.removeItem(at: url) }
        tempFileURL = nil
    }

    private func saveToPhotos() {
        UIImageWriteToSavedPhotosAlbum(image, nil, nil, nil)
    }

    private func utTypeForMime() -> UTType {
        guard let mime = mimeType else { return .png }
        if mime == "image/jpeg" { return .jpeg }
        if mime == "image/webp" { return .webP }
        if mime == "image/gif" { return .gif }
        if mime == "image/bmp" { return .bmp }
        if mime == "image/tiff" { return .tiff }
        return .png
    }

    private func sanitized(_ name: String) -> String {
        let invalid = CharacterSet(charactersIn: "/:\\?%*|\"<>")
        let cleaned = name.components(separatedBy: invalid).joined(separator: "_")
        return cleaned.isEmpty ? "attachment" : cleaned
    }
}

/// A trivial FileDocument so the .fileExporter modifier can write bytes
/// to a user-chosen Files destination.
private struct TempFileDocument: FileDocument {
    let url: URL

    static var readableContentTypes: [UTType] { [.png, .jpeg, .webP, .gif, .bmp, .tiff] }

    init(url: URL) { self.url = url }

    init(configuration: ReadConfiguration) throws { fatalError("read-only export") }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        try FileWrapper(regularFileWithContents: Data(contentsOf: url))
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
