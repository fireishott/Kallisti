import AVFoundation
import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Build 31: adaptive attachment picker — native detents on iPhone, popover on iPad,
/// scannable rows instead of tiny floating tiles, preparation progress, multi-select,
/// and Accessibility support.
struct AttachmentPickerSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @Environment(PermissionsStore.self) private var permissionsStore

    @State private var showCamera = false
    @State private var showPhotoPicker = false
    @State private var showVideoPicker = false
    @State private var showFilePicker = false
    @State private var selectedPhotoItems: [PhotosPickerItem] = []
    @State private var selectedVideoItem: PhotosPickerItem?
    @State private var isPreparing = false
    @State private var preparationError: String?

    /// Max items the user can select across all sources.
    private let maxSelectionCount = PendingAttachment.maxAttachmentsPerMessage

    var onAttachmentPicked: ((AttachmentResult) -> Void)?

    // MARK: - Body

    var body: some View {
        NavigationStack {
            List {
                // Camera
                if UIImagePickerController.isSourceTypeAvailable(.camera) {
                    sourceRow(
                        icon: "camera.fill",
                        color: .gray,
                        title: "Camera",
                        description: "Take a photo",
                        action: { showCamera = true }
                    )
                }

                // Photo Library
                sourceRow(
                    icon: "photo.on.rectangle.fill",
                    color: .blue,
                    title: "Photo Library",
                    description: "Choose from your photos",
                    action: { showPhotoPicker = true }
                )

                // Screen recordings are represented as a small sequence of
                // on-device frames, then sent via the gateway's proven native
                // image attachment path. The gateway has no video-byte RPC.
                sourceRow(
                    icon: "video.fill",
                    color: .orange,
                    title: "Screen Recording",
                    description: "Choose a video from your Photo Library",
                    action: { selectedVideoItem = nil; showVideoPicker = true }
                )

                // Browse Files
                sourceRow(
                    icon: "doc.fill",
                    color: .orange,
                    title: "Browse Files",
                    description: "Pick a document or image",
                    action: { showFilePicker = true }
                )
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Add Attachment")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Close") { dismiss() }
                        .font(Design.Typography.body.weight(.medium))
                }
            }
            .overlay {
                if isPreparing {
                    preparingOverlay
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .fullScreenCover(isPresented: $showCamera) {
            CameraPickerView { image in
                if let image {
                    prepareAndDeliver(.image(image))
                } else {
                    dismiss()
                }
            }
            .ignoresSafeArea()
        }
        .photosPicker(
            isPresented: $showPhotoPicker,
            selection: $selectedPhotoItems,
            maxSelectionCount: maxSelectionCount,
            matching: .images
        )
        .onChange(of: selectedPhotoItems) { _, items in
            guard !items.isEmpty else { return }
            preparePhotos(items)
        }
        .photosPicker(
            isPresented: $showVideoPicker,
            selection: $selectedVideoItem,
            matching: .videos
        )
        .onChange(of: selectedVideoItem) { _, item in
            guard let item else { return }
            prepareScreenRecording(item)
        }
        .sheet(isPresented: $showFilePicker) {
            DocumentPickerView(
                allowsMultipleSelection: maxSelectionCount > 1,
                onComplete: { urls in
                    if let url = urls.first {
                        prepareAndDeliver(.file(url))
                    } else {
                        dismiss()
                    }
                }
            )
        }
    }

    // MARK: - Source Row

    private func sourceRow(
        icon: String,
        color: Color,
        title: String,
        description: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 14) {
                Image(systemName: icon)
                    .font(.system(size: 22))
                    .foregroundStyle(color)
                    .frame(width: 36, height: 36)
                    .background(color.opacity(0.12))
                    .clipShape(RoundedRectangle(cornerRadius: 8))

                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(Design.Typography.body.weight(.medium))
                        .foregroundStyle(Design.Colors.foreground)
                    Text(description)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }

                Spacer()

                Image(systemName: "chevron.right")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.5))
            }
            .padding(.vertical, 6)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(title): \(description)")
        .accessibilityHint("Opens the \(title) picker")
    }

    // MARK: - Preparing Overlay

    private var preparingOverlay: some View {
        ZStack {
            Design.Colors.background.opacity(0.8)

            VStack(spacing: 16) {
                ProgressView()
                    .scaleEffect(1.2)

                Text("Preparing…")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)

                if let error = preparationError {
                    Text(error)
                        .font(Design.Typography.caption)
                        .foregroundStyle(.red)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 32)

                    Button("Cancel") {
                        isPreparing = false
                        preparationError = nil
                    }
                    .buttonStyle(.bordered)
                    .tint(.red)
                }
            }
            .padding(32)
            .background(
                RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                    .fill(Design.Colors.surface)
            )
            .shadow(radius: 16)
        }
        .transition(.opacity)
        .animation(Design.Motion.standard, value: isPreparing)
    }

    // MARK: - Delivery

    private func prepareAndDeliver(_ result: AttachmentResult) {
        isPreparing = true
        preparationError = nil

        Task {
            do {
                // Validate the attachment before delivering — a corrupt image
                // or inaccessible file should show an error, not silently dismiss.
                switch result {
                case .image(let image):
                    guard image.size.width > 0, image.size.height > 0,
                          image.cgImage != nil || image.ciImage != nil else {
                        throw PreparationError.invalidImage
                    }
                case .file(let url):
                    guard FileManager.default.isReadableFile(atPath: url.path) else {
                        throw PreparationError.inaccessibleFile
                    }
                    let resourceValues = try url.resourceValues(forKeys: [.fileSizeKey])
                    let fileSize = resourceValues.fileSize ?? 0
                    guard fileSize > 0 else {
                        throw PreparationError.emptyFile
                    }
                }
                onAttachmentPicked?(result)
                dismiss()
            } catch {
                preparationError = error.localizedDescription
            }
        }
    }

    /// Pull a recording out of Photos, sample it locally, then send the
    /// representative frames through the already-supported image attachment
    /// transport. This is intentional: the native gateway only accepts image
    /// bytes today, so we never present a video picker that silently drops data.
    private func prepareScreenRecording(_ item: PhotosPickerItem) {
        isPreparing = true
        preparationError = nil

        Task {
            do {
                guard let movie = try await item.loadTransferable(type: ScreenRecordingTransfer.self) else {
                    throw PreparationError.unreadableVideo
                }
                let frames = try await Self.representativeFrames(from: movie.url)
                guard !frames.isEmpty else { throw PreparationError.unreadableVideo }
                for frame in frames {
                    onAttachmentPicked?(.image(frame))
                }
                selectedVideoItem = nil
                dismiss()
            } catch {
                preparationError = error.localizedDescription
            }
        }
    }

    private static func representativeFrames(from url: URL) async throws -> [UIImage] {
        let asset = AVURLAsset(url: url)
        let duration = try await asset.load(.duration)
        let seconds = max(CMTimeGetSeconds(duration), 0)
        guard seconds.isFinite, seconds > 0 else { throw PreparationError.unreadableVideo }

        // Six evenly distributed frames give the agent enough temporal context
        // for an app recording without crushing the native image payload cap.
        let frameCount = min(6, max(1, Int(ceil(seconds))))
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 960, height: 960)
        var frames: [UIImage] = []
        for index in 0..<frameCount {
            let fraction = Double(index + 1) / Double(frameCount + 1)
            let time = CMTime(seconds: seconds * fraction, preferredTimescale: 600)
            if let cgImage = try? generator.copyCGImage(at: time, actualTime: nil) {
                frames.append(UIImage(cgImage: cgImage))
            }
        }
        return frames
    }

    private func preparePhotos(_ items: [PhotosPickerItem]) {
        isPreparing = true
        preparationError = nil

        Task {
            var delivered = false
            for item in items {
                guard !Task.isCancelled else { break }
                do {
                    if let data = try await item.loadTransferable(type: Data.self),
                       let image = UIImage(data: data) {
                        onAttachmentPicked?(.image(image))
                        delivered = true
                    }
                } catch {
                    preparationError = "Failed to load photo: \(error.localizedDescription)"
                }
            }
            selectedPhotoItems = []
            if delivered {
                dismiss()
            } else if preparationError == nil {
                preparationError = "No photos could be loaded."
            }
        }
    }

    private enum PreparationError: LocalizedError {
        case invalidImage
        case inaccessibleFile
        case emptyFile
        case unreadableVideo

        var errorDescription: String? {
            switch self {
            case .invalidImage: "The selected image could not be decoded."
            case .inaccessibleFile: "The selected file is not accessible."
            case .emptyFile: "The selected file is empty."
            case .unreadableVideo: "The screen recording could not be prepared."
            }
        }
    }
}

// MARK: - Attachment Result

enum AttachmentResult {
    case image(UIImage)
    case file(URL)
}

// MARK: - Camera Picker

struct CameraPickerView: UIViewControllerRepresentable {
    let onComplete: (UIImage?) -> Void

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ uiViewController: UIImagePickerController, context: Context) {}

    func makeCoordinator() -> Coordinator {
        Coordinator(onComplete: onComplete)
    }

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let onComplete: (UIImage?) -> Void

        init(onComplete: @escaping (UIImage?) -> Void) {
            self.onComplete = onComplete
        }

        func imagePickerController(
            _ picker: UIImagePickerController,
            didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
        ) {
            let image = info[.originalImage] as? UIImage
            onComplete(image)
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            onComplete(nil)
        }
    }
}

// MARK: - Document Picker

struct DocumentPickerView: UIViewControllerRepresentable {
    let allowsMultipleSelection: Bool
    let onComplete: ([URL]) -> Void

    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [
            .image, .plainText, .sourceCode, .json, .html, .xml, .yaml,
        ], asCopy: true)
        picker.allowsMultipleSelection = allowsMultipleSelection
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ uiViewController: UIDocumentPickerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator {
        Coordinator(onComplete: onComplete)
    }

    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        let onComplete: ([URL]) -> Void

        init(onComplete: @escaping ([URL]) -> Void) {
            self.onComplete = onComplete
        }

        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
            onComplete(urls)
        }

        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
            onComplete([])
        }
    }
}


/// File-based transfer keeps Photos' temporary movie URL valid long enough for
/// frame extraction. The copy is local and is never uploaded as opaque video
/// bytes, because the native gateway's supported attachment contract is images.
private struct ScreenRecordingTransfer: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .movie) { received in
            let source = received.file
            let ext = source.pathExtension.isEmpty ? "mov" : source.pathExtension
            let destination = FileManager.default.temporaryDirectory
                .appendingPathComponent("kallisti-screen-recording-\(UUID().uuidString).\(ext)")
            try FileManager.default.copyItem(at: source, to: destination)
            return ScreenRecordingTransfer(url: destination)
        }
    }
}
