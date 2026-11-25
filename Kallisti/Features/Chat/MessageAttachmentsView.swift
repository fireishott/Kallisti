import SwiftUI
import UIKit
import AVFoundation
import AVKit
import PDFKit
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
    /// The attachment open in the PDF editor, when the operator tapped one.
    @State private var editingPDF: ErisPDFTarget?
    @State private var fullScreenImage: FullScreenImagePayload?

    private var images: [MessageAttachment] { attachments.filter(\.isImage) }
    private var videos: [MessageAttachment] { attachments.filter { $0.kind == "video" } }
    private var pdfs: [MessageAttachment] { attachments.filter { $0.kind == "pdf" } }
    private var files: [MessageAttachment] { attachments.filter { !$0.isInlinePreview } }

    var body: some View {
        VStack(alignment: alignment, spacing: Design.Spacing.xs) {
            if !images.isEmpty {
                imageLayout
            }
            ForEach(videos) { video in
                VideoAttachmentView(attachment: video, maxWidth: 260, maxHeight: 200)
            }
            ForEach(pdfs) { pdf in
                PDFAttachmentView(attachment: pdf, maxWidth: 220, maxHeight: 280) {
                    Task { await openFile(pdf) }
                }
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
        .sheet(item: $editingPDF) { target in
            ErisPDFEditor(erisID: target.id, fileName: target.fileName)
        }
    }

    /// Whether this file came from the paired daemon rather than a relay.
    private func erisID(of attachment: MessageAttachment) -> String? {
        guard attachment.mediaURL?.scheme == "eris-attachment" else { return nil }
        return attachment.mediaURL?.host
    }

    @ViewBuilder
    private var imageLayout: some View {
        if images.count == 1, let id = erisID(of: images[0]) {
            // The daemon holds the bytes and serves them with the paired bearer,
            // so the file opens here instead of failing against a relay that never
            // had it.
            ErisAttachmentImage(erisID: id) { uiImage, data in
                fullScreenImage = FullScreenImagePayload(
                    image: uiImage,
                    fileName: images[0].fileName,
                    imageData: data,
                    mimeType: images[0].mimeType
                )
            }
            .frame(maxWidth: 260, maxHeight: 320)
            .clipShape(RoundedRectangle(cornerRadius: 10))
        } else if images.count == 1 {
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
                    if let id = erisID(of: image) {
                        ErisAttachmentImage(erisID: id) { uiImage, data in
                            fullScreenImage = FullScreenImagePayload(
                                image: uiImage,
                                fileName: image.fileName,
                                imageData: data,
                                mimeType: image.mimeType
                            )
                        }
                        .frame(maxWidth: 150, maxHeight: 150)
                        .clipShape(RoundedRectangle(cornerRadius: 10))
                    } else {
                    AttachmentImageView(attachment: image, maxWidth: 150, maxHeight: 150) { uiImage, data, mime in
                        fullScreenImage = FullScreenImagePayload(
                            image: uiImage, fileName: image.fileName,
                            imageData: data, mimeType: mime
                        )
                    }
                    }
                }
            }
            .frame(maxWidth: 320)
        }
    }

    private func openFile(_ attachment: MessageAttachment) async {
        // A file from the paired daemon comes over the device API with the bearer
        // the phone already holds; the relay download path would ask a host that
        // never had it, and the tap would do nothing.
        let data: Data?
        if let id = erisID(of: attachment) {
            data = try? await ErisDaemonStore.shared.clientForPairedDaemon()?.attachmentBytes(id: id)
        } else {
            data = await attachmentService.data(for: attachment)
        }
        guard let data else { return }
        // A PDF that came from the daemon opens where it can be filled in; Quick
        // Look shows a form but will not let a field be typed into.
        if attachment.mimeType == "application/pdf", let id = erisID(of: attachment) {
            editingPDF = ErisPDFTarget(id: id, fileName: attachment.fileName)
            return
        }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("KallistiPreview", isDirectory: true)
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

// MARK: - Inline video

/// Inline video preview: first-frame thumbnail with a play overlay. Tapping
/// plays the video full-screen. Bytes are fetched via AttachmentService, which
/// now supports native-gateway media URLs as well as the relay.
private struct VideoAttachmentView: View {
    let attachment: MessageAttachment
    let maxWidth: CGFloat
    let maxHeight: CGFloat

    @Environment(AttachmentService.self) private var attachmentService

    @State private var thumbnail: UIImage?
    @State private var loadFailed = false
    @State private var isPlaying = false
    @State private var localURL: URL?

    var body: some View {
        Group {
            if let thumbnail {
                Button {
                    isPlaying = true
                } label: {
                    ZStack {
                        Image(uiImage: thumbnail)
                            .resizable()
                            .aspectRatio(contentMode: .fill)
                            .frame(maxWidth: maxWidth, maxHeight: maxHeight)
                            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
                        Image(systemName: "play.circle.fill")
                            .font(.system(size: 44))
                            .foregroundStyle(.white)
                            .shadow(radius: 4)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Video")
                .accessibilityHint("Double-tap to play")
            } else if loadFailed {
                RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                    .fill(Design.Colors.surface)
                    .frame(width: min(maxWidth, 180), height: min(maxHeight, 140))
                    .overlay(
                        Image(systemName: "video.slash")
                            .font(.system(size: 18, weight: .medium))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    )
            } else {
                RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                    .fill(Design.Colors.surface)
                    .frame(width: min(maxWidth, 180), height: min(maxHeight, 140))
                    .overlay(ProgressView())
            }
        }
        .task { await loadThumbnail() }
        .fullScreenCover(isPresented: $isPlaying) {
            if let localURL {
                AttachmentVideoPlayerView(url: localURL)
            }
        }
    }

    private func loadThumbnail() async {
        guard let data = await attachmentService.data(for: attachment) else {
            loadFailed = true
            return
        }
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("KallistiVideo", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appendingPathComponent(attachment.fileName)
        do {
            try data.write(to: url, options: .atomic)
        } catch {
            loadFailed = true
            return
        }
        localURL = url
        let asset = AVAsset(url: url)
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        do {
            let cgImage = try generator.copyCGImage(at: .zero, actualTime: nil)
            thumbnail = UIImage(cgImage: cgImage)
        } catch {
            loadFailed = true
        }
    }
}

// MARK: - Inline PDF

/// Inline PDF preview: first-page thumbnail. Tapping opens the full document
/// via QuickLook (reusing the parent's `openFile` path).
private struct PDFAttachmentView: View {
    let attachment: MessageAttachment
    let maxWidth: CGFloat
    let maxHeight: CGFloat
    let onTap: () -> Void

    @Environment(AttachmentService.self) private var attachmentService

    @State private var thumbnail: UIImage?
    @State private var loadFailed = false

    var body: some View {
        Group {
            if let thumbnail {
                Button(action: onTap) {
                    Image(uiImage: thumbnail)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: maxWidth, maxHeight: maxHeight)
                        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
                        .overlay(
                            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                                .stroke(Design.Colors.divider, lineWidth: 1)
                        )
                }
                .buttonStyle(.plain)
                .accessibilityLabel("PDF document")
                .accessibilityHint("Double-tap to open")
            } else if loadFailed {
                RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                    .fill(Design.Colors.surface)
                    .frame(width: min(maxWidth, 180), height: min(maxHeight, 140))
                    .overlay(
                        Image(systemName: "doc.badge.exclamationmark")
                            .font(.system(size: 18, weight: .medium))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    )
            } else {
                RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                    .fill(Design.Colors.surface)
                    .frame(width: min(maxWidth, 180), height: min(maxHeight, 140))
                    .overlay(ProgressView())
            }
        }
        .task { await loadThumbnail() }
    }

    private func loadThumbnail() async {
        guard let data = await attachmentService.data(for: attachment),
              let document = PDFDocument(data: data),
              let page = document.page(at: 0) else {
            loadFailed = true
            return
        }
        let bounds = page.bounds(for: .mediaBox)
        let renderer = UIGraphicsImageRenderer(size: bounds.size)
        thumbnail = renderer.image { context in
            UIColor.white.set()
            context.fill(bounds)
            page.draw(with: .mediaBox, to: context.cgContext)
        }
    }
}

// MARK: - Video playback

private struct AttachmentVideoPlayerView: View {
    let url: URL
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack(alignment: .topTrailing) {
            VideoPlayer(player: AVPlayer(url: url))
                .ignoresSafeArea()
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 28))
                    .foregroundStyle(.white)
                    .padding()
            }
        }
        .background(Color.black)
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
            .appendingPathComponent("KallistiImageShare", isDirectory: true)
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


// MARK: - Files a turn attached

/// One file from an Eris turn, fetched over the device API.
///
/// The daemon holds the bytes and serves them with the paired bearer; the phone has
/// never needed a dashboard session for its own files. The message carries
/// eris-attachment://<id> as the handle.
///
/// This drew a bare `Image` with no gesture on it, so a photo the daemon attached
/// could be looked at and never opened: no viewer, no zoom, no share, no save. It
/// is a button now, and the bytes it already downloaded are handed to the viewer
/// so tapping does not re-fetch a multi-megabyte image over the tailnet.
struct ErisAttachmentImage: View {
    let erisID: String
    /// Called with the decoded image and its bytes when the thumbnail is tapped.
    var onTap: ((UIImage, Data) -> Void)?
    @State private var image: UIImage?
    @State private var bytes: Data?
    @State private var failed = false

    var body: some View {
        Group {
            if let image {
                if let onTap {
                    Button {
                        // The bytes are already here; re-downloading on tap would
                        // make a 12 MP photo feel broken on a slow link.
                        onTap(image, bytes ?? Data())
                    } label: {
                        Image(uiImage: image)
                            .resizable()
                            .scaledToFit()
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Attached image")
                    .accessibilityHint("Double-tap to view full screen")
                } else {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFit()
                }
            } else if failed {
                Label("This file could not be opened", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .task(id: erisID) {
            guard let client = ErisDaemonStore.shared.clientForPairedDaemon(),
                  let data = try? await client.attachmentBytes(id: erisID)
            else {
                failed = true
                return
            }
            bytes = data
            image = UIImage(data: data)
            failed = image == nil
        }
    }
}


// MARK: - A PDF a turn attached, opened for filling in

/// Which file the editor is showing.
struct ErisPDFTarget: Identifiable {
    let id: String
    let fileName: String
}

/// A PDF from a turn, with live form fields and a way to share what you typed.
///
/// PDFKit renders AcroForm fields as editable controls, so a form can be filled in
/// without a field editor of our own. "Save" writes the edited document - the
/// annotations PDFKit made are part of the file once written - and hands it to the
/// share sheet, where Save to Files, Mail and everything else lives.
struct ErisPDFEditor: View {
    let erisID: String
    let fileName: String

    @Environment(\.dismiss) private var dismiss
    @State private var document: PDFDocument?
    @State private var failure: String?
    @State private var shareURL: URL?
    @State private var filling = false
    /// Reading a page in a form sheet wastes the screen. This opens the same
    /// document edge to edge; the sheet (and its Save/share) stays underneath.
    @State private var isFullscreen = false

    /// How many form fields the document carries, so the toolbar can say the form
    /// is fillable instead of leaving the operator to discover it.
    private var filledFieldCount: Int {
        guard let document else { return 0 }
        var count = 0
        for index in 0..<document.pageCount {
            guard let page = document.page(at: index) else { continue }
            for annotation in page.annotations where annotation.widgetFieldType != nil {
                count += 1
            }
        }
        return count
    }

    var body: some View {
        NavigationStack {
            Group {
                if let document {
                    PDFKitView(document: document)
                } else if let failure {
                    ContentUnavailableView("Could not open this PDF", systemImage: "doc", description: Text(failure))
                } else {
                    ProgressView()
                }
            }
            .navigationTitle(fileName)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItemGroup(placement: .primaryAction) {
                    Button {
                        isFullscreen = true
                    } label: {
                        Image(systemName: "arrow.up.left.and.arrow.down.right")
                    }
                    .disabled(document == nil)
                    .accessibilityLabel("Full screen")
                    if filledFieldCount > 0 {
                        Label("\(filledFieldCount) fields", systemImage: "square.and.pencil")
                            .labelStyle(.iconOnly)
                            .foregroundStyle(.secondary)
                    }
                    Button {
                        Task { await prepareShare() }
                    } label: {
                        Image(systemName: "square.and.arrow.up")
                    }
                    .disabled(document == nil || filling)
                }
            }
            .task { await load() }
            .fullScreenCover(isPresented: $isFullscreen) {
                if let document {
                    ErisPDFFullscreen(document: document, fileName: fileName)
                }
            }
            .sheet(item: Binding(
                get: { shareURL.map(IdentifiableURL.init) },
                set: { _ in shareURL = nil }
            )) { wrapped in
                ShareSheet(items: [wrapped.url])
            }
        }
    }

    private func load() async {
        guard let client = ErisDaemonStore.shared.clientForPairedDaemon() else {
            failure = "this phone is not paired with a daemon"
            return
        }
        do {
            let data = try await client.attachmentBytes(id: erisID)
            guard let pdf = PDFDocument(data: data) else {
                failure = "the bytes were not a PDF this phone can read"
                return
            }
            document = pdf
        } catch {
            failure = (error as? ErisError)?.message ?? error.localizedDescription
        }
    }

    /// Write what has been typed, then offer it to the share sheet.
    private func prepareShare() async {
        guard let document else { return }
        filling = true
        defer { filling = false }
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("KallistiPreview", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appendingPathComponent(fileName.isEmpty ? "document.pdf" : fileName)
        guard document.write(to: url) else {
            failure = "the edited PDF could not be written"
            return
        }
        shareURL = url
    }
}

/// PDFKit's view, with form fields live and pages fitted.
/// The PDF at full screen, for reading. The form editor stays in the sheet
/// underneath and owns Save/share; this is the page at a useful size.
private struct ErisPDFFullscreen: View {
    let document: PDFDocument
    let fileName: String

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            PDFKitView(document: document)
                .ignoresSafeArea(edges: .bottom)
                .navigationTitle(fileName)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Done") { dismiss() }
                    }
                }
        }
    }
}

struct PDFKitView: UIViewRepresentable {
    let document: PDFDocument

    func makeUIView(context: Context) -> PDFView {
        let view = PDFView()
        view.document = document
        view.autoScales = true
        view.displayMode = .singlePageContinuous
        view.displayDirection = .vertical
        return view
    }

    func updateUIView(_ view: PDFView, context: Context) {
        if view.document !== document { view.document = document }
    }
}


