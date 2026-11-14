import Testing
import Foundation
import Speech
@testable import Kallisti

/// One-shot gate for the two racers below.
private final class Once: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false

    func claim() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !done else { return false }
        done = true
        return true
    }
}

@Suite("Speech Authorization Bridge")
struct SpeechAuthorizationBridgeTests {
    @Test("Bridge compiles and is callable from non-MainActor context")
    func bridgeCallableFromOffActor() async {
        // Compile-time verification: the bridge is `nonisolated` and can be
        // called from a non-@MainActor async context without isolation inference.
        // We cannot assert the TCC dialog response (pure unit tests cannot
        // present system dialogs), but if this compiles and runs, the MainActor
        // inheritance that caused the build 30 crash is gone.
        // No TCC answer is guaranteed on a simulator: requestAuthorization can
        // stay pending forever when the prompt is never answered, which hung the
        // whole serial suite until the external watchdog killed it. A task group
        // is not enough — it joins its children at scope exit, and the cancelled
        // authorization call is not cancellation-aware — so the wait is bounded
        // by a continuation that settles once, on whichever side arrives first.
        let settled = Once()
        let status: SFSpeechRecognizerAuthorizationStatus? = await withCheckedContinuation { continuation in
            Task {
                let resolved = await SpeechAuthorizationBridge.requestAuthorization()
                if settled.claim() { continuation.resume(returning: resolved) }
            }
            Task {
                try? await Task.sleep(for: .seconds(15))
                if settled.claim() { continuation.resume(returning: nil) }
            }
        }
        // status is .notDetermined, .authorized, .denied, or .restricted —
        // all valid, and nil means the prompt never settled.
        if let status {
            let valid = (status == .notDetermined || status == .authorized
                         || status == .denied || status == .restricted)
            #expect(valid, "Speech authorization returned a valid enum case")
        }
    }
}

@Suite("Speech Service Availability")
@MainActor
struct SpeechServiceTests {
    @Test("Speech recognition returns unsupported on pre-iOS 26")
    func speechRecognitionStatusUnsupported() {
        // On the current OS version (which may be < iOS 26), verify that
        // speech recognition is reported as unsupported when the OS doesn't
        // support the modern Speech APIs.
        let status = PermissionsStore.speechRecognitionAvailabilityStatus()

        if #available(iOS 26.0, *) {
            // On iOS 26+, speech recognition should be available (not unsupported)
            #expect(status != .unsupported, "Speech should be available on iOS 26+")
        } else {
            // On iOS < 26, speech recognition should be unsupported
            #expect(status == .unsupported, "Speech should be unsupported on iOS < 26")
        }
    }

    @Test("Speech service factory returns nil on pre-iOS 26")
    func speechServiceFactoryReturnsNil() {
        let service = createSpeechDictationService()

        if #available(iOS 26.0, *) {
            #expect(service != nil, "Speech service should be created on iOS 26+")
        } else {
            #expect(service == nil, "Speech service should be nil on iOS < 26")
        }
    }

    @Test("Speech recognition capability shows unsupported status detail on older iOS")
    func speechRecognitionCapabilityDetail() {
        let status = PermissionsStore.speechRecognitionAvailabilityStatus()

        if #available(iOS 26.0, *) {
            // On iOS 26+, no special detail needed
            let detail = PermissionsStore.speechRecognitionStatusDetail(for: status)
            // detail may be nil (normal flow) or contain authorization info
        } else {
            // On iOS < 26, should show clear explanation
            let detail = PermissionsStore.speechRecognitionStatusDetail(for: status)
            #expect(detail != nil, "Should have a status detail on unsupported OS")
            #expect(
                detail?.contains("iOS 26") == true || detail?.contains("not available") == true,
                "Detail should mention iOS 26 requirement or unavailability"
            )
        }
    }
}
