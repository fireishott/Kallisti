import UserNotifications

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

        // Ensure deep-link fields are present for tap handling
        if bestAttemptContent.userInfo["conversationId"] == nil {
            // If the push didn't include conversationId, try to extract from the payload
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
