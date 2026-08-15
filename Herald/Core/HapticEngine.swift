import UIKit

/// Centralized haptic feedback utility.
///
/// All haptic methods are MainActor-isolated since `UIFeedbackGenerator`
/// must be used from the main thread.
@MainActor
enum HapticEngine {
    // Pre-warmed generators for reliable haptic delivery.
    // Calling prepare() spins up the Taptic Engine so it's ready
    // when impactOccurred() fires — prevents dropped haptics during
    // heavy UI layout passes.
    private static let lightGenerator: UIImpactFeedbackGenerator = {
        let gen = UIImpactFeedbackGenerator(style: .light)
        gen.prepare()
        return gen
    }()

    private static let mediumGenerator: UIImpactFeedbackGenerator = {
        let gen = UIImpactFeedbackGenerator(style: .medium)
        gen.prepare()
        return gen
    }()

    private static let notificationGenerator = UINotificationFeedbackGenerator()

    /// Light impact when a message is sent.
    static func messageSent() {
        lightGenerator.impactOccurred()
        lightGenerator.prepare()  // Re-prepare for next use
    }

    /// Medium impact when a streaming response completes.
    static func responseReceived() {
        mediumGenerator.impactOccurred()
        mediumGenerator.prepare()  // Re-prepare for next use
    }

    /// Light impact when a new chat is created.
    static func newChat() {
        lightGenerator.impactOccurred()
        lightGenerator.prepare()  // Re-prepare for next use
    }

    /// Error notification for failed operations.
    static func error() {
        notificationGenerator.notificationOccurred(.error)
    }
}
