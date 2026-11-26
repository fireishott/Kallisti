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

    /// Maximum file size: 350 KB (before base64 encoding -> ~470KB base64).
    /// The most bytes one attachment may carry.
    ///
    /// This was 350 KB, with a comment about a 1 MB Herald relay request body. That
    /// relay is gone: the Eris daemon takes the file as RAW BYTES on its own
    /// `POST /v1/attachments`, and its ceiling is 25 MB per file with a 64 MB total
    /// per message. Nothing is base64'd into a chat request any more, so the old
    /// number was compressing photos for a limit that no longer exists.
    ///
    /// Matched to the daemon's `chat::MAX_ATTACHMENT_BYTES` so the app refuses a
    /// file the daemon would refuse, and nothing smaller.
    static let maxFileSize = 25 * 1024 * 1024

    /// The most files one message may carry.
    ///
    /// Ten, and the daemon's `chat::MAX_ATTACHMENTS` is ten: the two agreed on
    /// different numbers once, so the picker offered ten photos and the daemon
    /// refused the message with `attachments_invalid: a message may carry at most 8
    /// files`. A limit the UI shows must be the limit the server honours, and the
    /// operator's expectation - ten - is the one that was reasonable.
    ///
    /// These two numbers must move together or not at all.
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

    static func supportsMimeType(_ mimeType: String) -> Bool {
        mimeType.hasPrefix("image/") || supportedTextMimeTypes.contains(mimeType)
            || supportedVideoMimeTypes.contains(mimeType)
    }

    /// Create an image attachment from a UIImage, at full size and resolution.
    ///
    /// This used to downscale every photo to 768 points on its longest edge and
    /// encode it at quality 0.5, then keep stepping the quality down until the file
    /// fit in 350 KB. A 12-megapixel photo arrived at the daemon as roughly a
    /// 0.2-megapixel thumbnail: useless for anything visual, and destructive in a
    /// way nobody asked for. The limits it was protecting against - a 1 MB relay
    /// body carrying base64 - no longer exist, and the daemon takes raw bytes up to
    /// 25 MB.
    ///
    /// So the pixels are passed through untouched. The image is re-encoded only to
    /// pick a container the daemon recognises, and at the highest quality rather
    /// than a chosen one: `UIImage.jpegData(quality: 1.0)` is visually lossless for
    /// a camera image and much smaller than PNG for the same picture. An image
    /// already inside `maxFileSize` is never re-encoded at all.
    static func image(_ image: UIImage, fileName: String? = nil) -> PendingAttachment? {
        // The original bytes when the caller has them (a file picker or
        // PhotosPicker hands over a file, not a UIImage) are preserved exactly;
        // this path only runs for an in-memory UIImage, where the best available
        // fidelity is one high-quality re-encode.
        guard let jpegData = image.jpegData(compressionQuality: 1.0) else { return nil }

        // A genuine overshoot is refused rather than silently crushed. The caller
        // gets nil and the UI says the file is too large; shipping a degraded copy
        // the operator did not ask for is the bug this replaced.
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

    /// Build an image attachment from the bytes a photo picker handed over.
    ///
    /// The picker gives `Data` and the old code decoded it to a `UIImage`, threw the
    /// bytes away, and re-encoded at quality 0.5. That is a lossy round trip over a
    /// file that was already the operator's picture, and it is what "compressed on
    /// upload" meant. The bytes are kept as they arrived; the decoded image is used
    /// only to draw the thumbnail.
    ///
    /// The media type comes from the bytes, not from the file extension, because a
    /// picker can hand over HEIC and the daemon sniffs bytes anyway.
    static func imageData(_ data: Data, fileName: String? = nil) -> PendingAttachment? {
        guard let image = UIImage(data: data) else { return nil }
        guard data.count <= maxFileSize else { return nil }
        let name = fileName ?? "photo_\(UUID().uuidString.prefix(8)).heic"
        return PendingAttachment(
            kind: .image,
            fileName: name,
            mimeType: Self.imageMimeType(for: data),
            data: data,
            localStoragePath: stageLocally(data: data, preferredFileName: name),
            thumbnailData: croppedThumbnailData(from: image)
        )
    }

    /// The media type of image bytes, decided by magic bytes.
    ///
    /// Mirrors the daemon's `chat::sniff_media_type` for the formats a phone
    /// produces. Guessing `image/jpeg` for a HEIC is the kind of label mismatch that
    /// makes a model describe garbage, so the signature decides.
    private static func imageMimeType(for data: Data) -> String {
        let bytes = [UInt8](data)
        /// Whether `signature` sits at `offset`.
        ///
        /// Explicit bounds checking rather than a slice comparison: `bytes[0..4]`
        /// is an `ArraySlice`, and comparing one to a literal array is not an
        /// operation Swift defines - it is a compile error, not a runtime miss.
        func matches(_ signature: [UInt8], at offset: Int = 0) -> Bool {
            guard bytes.count >= offset + signature.count else { return false }
            return Array(bytes[offset..<(offset + signature.count)]) == signature
        }
        if matches([0x89, 0x50, 0x4E, 0x47]) { return "image/png" }
        if matches([0xFF, 0xD8, 0xFF]) { return "image/jpeg" }
        if matches(Array("RIFF".utf8)), matches(Array("WEBP".utf8), at: 8) { return "image/webp" }
        if matches(Array("GIF".utf8)) { return "image/gif" }
        if matches(Array("ftyp".utf8), at: 4) {
            let brand = String(decoding: bytes[8..<12], as: UTF8.self)
            if ["heic", "heix", "hevc", "hevx", "heim", "heis", "mif1", "msf1"].contains(brand) {
                return "image/heic"
            }
        }
        return "image/jpeg"
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
        let isVideo = supportedVideoMimeTypes.contains(mimeType)

        // An image that arrived as a FILE keeps its own bytes, verbatim.
        //
        // This used to decode it to a UIImage and hand it to `image(...)`, which
        // re-encoded it - and before that fix, downscaled it to 768 pixels. A HEIC
        // straight out of Photos is the operator's original; decoding and
        // re-encoding it can only lose, and it turns a 2 MB HEIC into a much larger
        // JPEG for no gain. The bytes are the picture.
        if isImage, let image = UIImage(data: data) {
            guard data.count <= maxFileSize else { return nil }
            return PendingAttachment(
                kind: .image,
                fileName: url.lastPathComponent,
                mimeType: mimeType,
                data: data,
                localStoragePath: stageLocally(data: data, preferredFileName: url.lastPathComponent),
                thumbnailData: croppedThumbnailData(from: image)
            )
        }

        guard data.count <= (isVideo ? maxVideoFileSize : maxFileSize) else { return nil }

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
        guard let data = try? Data(contentsOf: url), data.count <= maxFileSize else { return nil }

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
