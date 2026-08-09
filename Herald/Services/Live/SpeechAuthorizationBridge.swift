import Foundation
import Speech

/// Bridges `SFSpeechRecognizer.requestAuthorization(_:)` into async/await without
/// inheriting MainActor isolation.
///
/// TCC delivers the handler on an arbitrary XPC/dispatch queue, and the SDK does not
/// mark it `NS_SWIFT_SENDABLE` (Speech.framework/Headers/SFSpeechRecognizer.h:115).
/// A closure written inside a `@MainActor` member therefore inherits MainActor
/// isolation, and Swift 6 emits a `swift_task_checkIsolated` assertion in its prologue
/// that traps on the callback thread — this was the build 30 crash (EXC_BREAKPOINT).
///
/// `nonisolated` removes the inherited isolation; `@Sendable` on the handler prevents
/// it from being re-inferred. Both are load-bearing. Do not "simplify" this back into
/// an inline closure.
enum SpeechAuthorizationBridge {
    nonisolated static func requestAuthorization()
        async -> SFSpeechRecognizerAuthorizationStatus
    {
        await withCheckedContinuation { (continuation: CheckedContinuation<SFSpeechRecognizerAuthorizationStatus, Never>) in
            SFSpeechRecognizer.requestAuthorization { @Sendable status in
                continuation.resume(returning: status)
            }
        }
    }
}
