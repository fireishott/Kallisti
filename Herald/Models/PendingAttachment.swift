import Foundation
import UIKit

/// An attachment staged in the composer before sending.
struct PendingAttachment: Identifiable, Sendable {
    let id = UUID()
    let kind: Kind
    let fileName: String
    let mimeType: String
    let data: Data
    let localStoragePath: String?
    /// Thumbnail for display — stored separately since UIImage isn't Sendable.
    let thumbnailData: Data?

    enum Kind: String, Sendable {
        case image
        case file
    }

    /// Per-file ceiling for anything that is not a document or a video.
    ///
    /// The old 350 KB was a relay limit: the Hermes API took base64 inside one
    /// message body, so a file had to fit an encoded budget. On Eris the bytes go
    /// up raw in their own request and the daemon's own ceiling is 25 MB, so the
    /// client was refusing zips, archives, logs and office documents the daemon
    /// would happily store. Set just under the daemon's cap so a refusal comes from
    /// the side that owns the limit, with a typed error.
    static let maxFileSize = 24 * 1024 * 1024
    /// Documents keep their original bytes (no downscaling path). Same ceiling and
    /// same reason as [`maxFileSize`].
    static let maxDocumentFileSize = 24 * 1024 * 1024
    static let maxAttachmentsPerMessage = 10
    /// Build 78.7: video files ride the file.attach data_url path (WS cap 384 MB).
    static let maxVideoFileSize = 150 * 1024 * 1024

    private static let supportedVideoMimeTypes: Set<String> = [
        "video/mp4",
        "video/quicktime",
        "video/x-m4v",
        "video/x-msvideo",
        "video/3gpp",
    ]

    private static let supportedTextMimeTypes: Set<String> = [
        "text/plain",
        "text/csv",
        "text/markdown",
        "text/html",
        "text/xml",
        "text/x-python",
        "text/x-swift",
        "text/javascript",
        "application/json",
        "application/xml",
        "application/yaml",
        "application/x-yaml",
    ]

    private static let supportedDocumentMimeTypes: Set<String> = [
        "application/pdf",
    ]

    /// Whether the composer will stage a file of this type.
    ///
    /// Everything, now. The list this replaced was the relay's: text, images, video
    /// and PDF, so a zip or a spreadsheet was dropped in the picker with no
    /// explanation. On Eris the daemon sniffs the bytes and serves whatever it
    /// stored, so the honest policy is "any type" and the one real limit is size.
    static func supportsMimeType(_ mimeType: String) -> Bool {
        !mimeType.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// Per-type raw byte ceiling. Images are downscaled before this is checked,
    /// documents/videos keep their original bytes.
    static func maxSize(forMimeType mimeType: String) -> Int {
        if supportedVideoMimeTypes.contains(mimeType) { return maxVideoFileSize }
        if supportedDocumentMimeTypes.contains(mimeType) { return maxDocumentFileSize }
        return maxFileSize
    }

    /// Create an image attachment from a UIImage.
    /// Large images are automatically downscaled to stay within the size limit.
    static func image(_ image: UIImage, fileName: String? = nil) -> PendingAttachment? {
        var quality: CGFloat = 0.5
        var targetImage = image

        // Downscale images to 768px max — keeps base64 well under API body limit
        let maxDimension: CGFloat = 768
        if max(image.size.width, image.size.height) > maxDimension {
            let scale = maxDimension / max(image.size.width, image.size.height)
            let newSize = CGSize(width: image.size.width * scale, height: image.size.height * scale)
            let renderer = UIGraphicsImageRenderer(size: newSize)
            targetImage = renderer.image { _ in
                image.draw(in: CGRect(origin: .zero, size: newSize))
            }
        }

        guard var jpegData = targetImage.jpegData(compressionQuality: quality) else { return nil }

        // Progressively lower quality if still too large
        while jpegData.count > maxFileSize && quality > 0.1 {
            quality -= 0.2
            if let reduced = targetImage.jpegData(compressionQuality: max(quality, 0.1)) {
                jpegData = reduced
            } else {
                break
            }
        }
        guard jpegData.count <= maxFileSize else { return nil }

        // Build 114: generate an aspect-fill crop, not an aspect-fit square.
        // Aspect-fit baked blank pixels into thumbnailBase64, producing white
        // bars until preview fetched the uncropped original image.
        let thumbData = croppedThumbnailData(from: image)

        return PendingAttachment(
            kind: .image,
            fileName: fileName ?? "photo_\(UUID().uuidString.prefix(8)).jpg",
            mimeType: "image/jpeg",
            data: jpegData,
            localStoragePath: stageLocally(data: jpegData, preferredFileName: fileName ?? "photo.jpg"),
            thumbnailData: thumbData
        )
    }

    static func croppedThumbnailData(from image: UIImage) -> Data? {
        let thumbSize = CGSize(width: 120, height: 120)
        guard image.size.width > 0, image.size.height > 0 else { return nil }
        let scale = max(thumbSize.width / image.size.width, thumbSize.height / image.size.height)
        let scaledSize = CGSize(width: image.size.width * scale, height: image.size.height * scale)
        let origin = CGPoint(
            x: (thumbSize.width - scaledSize.width) / 2,
            y: (thumbSize.height - scaledSize.height) / 2
        )
        let renderer = UIGraphicsImageRenderer(size: thumbSize)
        let thumbnail = renderer.image { context in
            context.cgContext.setFillColor(UIColor.clear.cgColor)
            context.cgContext.fill(CGRect(origin: .zero, size: thumbSize))
            image.draw(in: CGRect(origin: origin, size: scaledSize))
        }
        return thumbnail.jpegData(compressionQuality: 0.6)
    }

    /// Create a file attachment from a URL.
    static func file(at url: URL) -> PendingAttachment? {
        let mimeType = Self.mimeType(for: url)
        guard supportsMimeType(mimeType) else { return nil }

        guard let data = try? Data(contentsOf: url) else { return nil }
        let isImage = mimeType.hasPrefix("image/")

        if isImage, let image = UIImage(data: data) {
            return Self.image(image, fileName: url.lastPathComponent)
        }

        guard data.count <= Self.maxSize(forMimeType: mimeType) else { return nil }

        var thumbData: Data?
        if let image = UIImage(data: data) {
            let thumbSize = CGSize(width: 120, height: 120)
            let renderer = UIGraphicsImageRenderer(size: thumbSize)
            let thumbImage = renderer.image { _ in
                image.draw(in: CGRect(origin: .zero, size: thumbSize))
            }
            thumbData = thumbImage.jpegData(compressionQuality: 0.6)
        }

        return PendingAttachment(
            kind: isImage ? .image : .file,
            fileName: url.lastPathComponent,
            mimeType: mimeType,
            data: data,
            localStoragePath: stageLocally(data: data, preferredFileName: url.lastPathComponent),
            thumbnailData: thumbData
        )
    }

    static func restore(from attachment: MessageAttachment) -> PendingAttachment? {
        guard let localStoragePath = attachment.localStoragePath else { return nil }
        let url = URL(fileURLWithPath: localStoragePath)
        guard let data = try? Data(contentsOf: url),
              data.count <= Self.maxSize(forMimeType: attachment.mimeType) else { return nil }

        let thumbnailData = attachment.thumbnailBase64.flatMap { Data(base64Encoded: $0) }
        let kind = attachment.kind == "image" ? Kind.image : Kind.file
        return PendingAttachment(
            kind: kind,
            fileName: attachment.fileName,
            mimeType: attachment.mimeType,
            data: data,
            localStoragePath: localStoragePath,
            thumbnailData: thumbnailData
        )
    }

    /// Base64 encoded data string.
    var base64Data: String {
        data.base64EncodedString()
    }

    var thumbnailBase64: String? {
        thumbnailData?.base64EncodedString()
    }

    static func mimeType(for url: URL) -> String {
        let ext = url.pathExtension.lowercased()
        let map: [String: String] = [
            "jpg": "image/jpeg", "jpeg": "image/jpeg", "png": "image/png",
            "gif": "image/gif", "webp": "image/webp", "heic": "image/heic",
            "txt": "text/plain",
            "pdf": "application/pdf",
            "json": "application/json", "csv": "text/csv",
            "md": "text/markdown", "swift": "text/x-swift",
            "py": "text/x-python", "js": "text/javascript",
            "html": "text/html", "css": "text/css",
            "xml": "text/xml", "yml": "application/yaml",
            "yaml": "application/yaml",
            "mp4": "video/mp4",
            "mov": "video/quicktime",
            "m4v": "video/x-m4v",
            "avi": "video/x-msvideo",
            "3gp": "video/3gpp",
            // Archives, office documents and the rest of what a desktop hands you.
            "zip": "application/zip", "gz": "application/gzip",
            "tar": "application/x-tar", "7z": "application/x-7z-compressed",
            "rar": "application/vnd.rar", "dmg": "application/x-apple-diskimage",
            "doc": "application/msword",
            "docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "xls": "application/vnd.ms-excel",
            "xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "ppt": "application/vnd.ms-powerpoint",
            "pptx": "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "rtf": "application/rtf",
            "log": "text/plain", "sh": "text/x-shellscript",
            "toml": "application/toml", "ini": "text/plain",
            "sql": "text/plain", "ts": "text/typescript",
        ]
        return map[ext] ?? "application/octet-stream"
    }

    static func stageLocally(data: Data, preferredFileName: String) -> String? {
        let fileManager = FileManager.default
        guard let baseDirectory = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else {
            return nil
        }

        let attachmentDirectory = baseDirectory
            .appendingPathComponent("Kallisti", isDirectory: true)
            .appendingPathComponent("Attachments", isDirectory: true)

        do {
            try fileManager.createDirectory(at: attachmentDirectory, withIntermediateDirectories: true, attributes: nil)
            let sanitizedName = sanitizeFileName(preferredFileName)
            let destination = attachmentDirectory.appendingPathComponent("\(UUID().uuidString)-\(sanitizedName)")
            try data.write(to: destination, options: .atomic)
            return destination.path
        } catch {
            return nil
        }
    }

    private static func sanitizeFileName(_ fileName: String) -> String {
        let invalidCharacters = CharacterSet(charactersIn: "/:\\?%*|\"<>")
        let cleaned = fileName.components(separatedBy: invalidCharacters).joined(separator: "_")
        return cleaned.isEmpty ? "attachment" : cleaned
    }
}
