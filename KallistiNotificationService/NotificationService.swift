import UserNotifications
import UIKit
import KallistiSupport

class NotificationService: UNNotificationServiceExtension {
    private static let logger = "NotificationService"

    var contentHandler: ((UNNotificationContent) -> Void)?
    var bestAttemptContent: UNMutableNotificationContent?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        self.contentHandler = contentHandler
        bestAttemptContent = (request.content.mutableCopy() as? UNMutableNotificationContent)

        guard let bestAttemptContent else {
            contentHandler(request.content)
            return
        }

        // ── Format push notifications based on type ──────────────────
        let userInfo = bestAttemptContent.userInfo
        let pushType = userInfo["pushType"] as? String ?? userInfo["type"] as? String

        switch pushType {
        case "job.completed":
            bestAttemptContent.title = "Herald Response"
            // First 200 chars of response as body preview
            let response = userInfo["response"] as? String ?? userInfo["resultText"] as? String ?? ""
            bestAttemptContent.body = String(response.prefix(200))
            // Full response available when user opens the app
            if !response.isEmpty {
                bestAttemptContent.userInfo["full_response"] = response
            }
            bestAttemptContent.sound = .default

        case "job.failed":
            bestAttemptContent.title = "Herald Error"
            let errorText = userInfo["error"] as? String ?? userInfo["errorText"] as? String ?? "Request failed"
            bestAttemptContent.body = String(errorText.prefix(200))
            bestAttemptContent.sound = .default

        case "gateway.alert":
            bestAttemptContent.title = "\u{26A0}\u{FE0F} Gateway Alert"
            bestAttemptContent.body = userInfo["message"] as? String ?? "Alert"
            if let level = userInfo["level"] as? String, level == "critical" {
                bestAttemptContent.sound = .defaultCritical
            } else {
                bestAttemptContent.sound = .default
            }

        case "gateway.disconnected":
            bestAttemptContent.title = "Gateway Disconnected"
            bestAttemptContent.body = "Herald lost connection to Hermes. Tap to investigate."
            bestAttemptContent.sound = .default

        case "gateway.reconnected":
            bestAttemptContent.title = "Gateway Reconnected"
            bestAttemptContent.body = userInfo["message"] as? String ?? "Connection restored."
            bestAttemptContent.sound = .default

        default:
            // Existing behavior: ensure deep-link fields
            break
        }

        // Attach remote image attachment if present in push payload.
        // Build 83: the connector media route requires native auth, so a bare
        // URLSession download gets 401. Read the shared Keychain token via
        // KallistiSupport and attach an Authorization header.
        // Build 131.24: PDF attachments get rendered to a PNG thumbnail first.
        // iOS renders PDF notification thumbnails 180-degrees rotated (portrait
        // page quirk); image thumbnails are always oriented correctly.
        if let mediaUrlString = userInfo["mediaUrl"] as? String ?? userInfo["imageUrl"] as? String,
           let mediaUrl = URL(string: mediaUrlString) {
            let semaphore = DispatchSemaphore(value: 0)
            var request = URLRequest(url: mediaUrl)
            if let token = try? SharedCredentialProvider.shared.accessToken() {
                request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            }
            let task = URLSession.shared.downloadTask(with: request) { location, response, error in
                defer { semaphore.signal() }
                guard let location = location, error == nil else { return }

                let tmpDir = FileManager.default.temporaryDirectory
                let ext = mediaUrl.pathExtension.lowercased()

                // PDF: render first page to a PNG so the notification thumbnail
                // is oriented correctly.
                if ext == "pdf" {
                    guard let pdfDoc = CGPDFDocument(location as CFURL) else { return }
                    guard let page = pdfDoc.page(at: 1) else { return }

                    let pageRect = page.getBoxRect(.mediaBox)
                    // Cap thumbnail dimension so the push attachment stays small.
                    let maxDim: CGFloat = 1024
                    let scale = min(1.0, maxDim / max(pageRect.width, pageRect.height))
                    let size = CGSize(width: pageRect.width * scale,
                                      height: pageRect.height * scale)

                    let renderer = UIGraphicsImageRenderer(size: size)
                    let image = renderer.image { ctx in
                        UIColor.black.setFill()
                        ctx.fill(CGRect(origin: .zero, size: size))
                        ctx.cgContext.saveGState()
                        ctx.cgContext.scaleBy(x: scale, y: scale)
                        ctx.cgContext.translateBy(x: -pageRect.origin.x, y: -pageRect.origin.y)
                        ctx.cgContext.drawPDFPage(page)
                        ctx.cgContext.restoreGState()
                    }
                    guard let pngData = image.pngData() else { return }
                    let tmpFile = tmpDir.appendingPathComponent(UUID().uuidString + ".png")
                    do {
                        try pngData.write(to: tmpFile)
                        let attachment = try UNNotificationAttachment(identifier: "media", url: tmpFile, options: nil)
                        bestAttemptContent.attachments = [attachment]
                    } catch {}
                    return
                }

                let tmpFile = tmpDir.appendingPathComponent(UUID().uuidString + "." + (ext.isEmpty ? "jpg" : ext))
                do {
                    try FileManager.default.moveItem(at: location, to: tmpFile)
                    let attachment = try UNNotificationAttachment(identifier: "media", url: tmpFile, options: nil)
                    bestAttemptContent.attachments = [attachment]
                } catch {}
            }
            task.resume()
            _ = semaphore.wait(timeout: .now() + 5.0)
        }

        // Ensure deep-link fields are present for tap handling
        if bestAttemptContent.userInfo["conversationId"] == nil {
            if let convId = bestAttemptContent.targetContentIdentifier {
                bestAttemptContent.userInfo["conversationId"] = convId
            }
        }

        contentHandler(bestAttemptContent)
    }

    override func serviceExtensionTimeWillExpire() {
        // Called just before the extension will be terminated by the system.
        // Use this as an opportunity to deliver the best content possible.
        if let contentHandler, let bestAttemptContent {
            contentHandler(bestAttemptContent)
        }
    }
}
