import AVFoundation
import Foundation
import Speech

@MainActor
@Observable
final class PermissionsStore {
    var capabilities: [DeviceCapability] = []

    private let locationService: any LocationServiceProtocol
    private let healthService: any HealthServiceProtocol
    private let notificationService: any NotificationServiceProtocol
    private let mediaService: any MediaServiceProtocol
    private let motionService: LiveMotionService?

    /// Called on iOS 26+ when the user taps Allow for speech recognition.
    /// SFSpeechRecognizer.requestAuthorization crashes on early iOS 26 betas
    /// (FB-prefixed radar), so we rely on the new SpeechAnalyzer/DictationTranscriber
    /// APIs to trigger the TCC dialog. This closure should instantiate and immediately
    /// discard a speech recognizer to prompt the system dialog.
    var speechAuthorizationTrigger: (@MainActor () async -> Void)?

    init(
        locationService: any LocationServiceProtocol,
        healthService: any HealthServiceProtocol,
        notificationService: any NotificationServiceProtocol,
        mediaService: any MediaServiceProtocol,
        motionService: LiveMotionService? = nil
    ) {
        self.locationService = locationService
        self.healthService = healthService
        self.notificationService = notificationService
        self.mediaService = mediaService
        self.motionService = motionService
        self.capabilities = currentCapabilities()
    }

    func reloadCapabilities() async {
        locationService.refreshAuthorizationState()
        await healthService.refreshAuthorizationStatus()
        await notificationService.refreshAuthorizationStatus()
        motionService?.refreshAuthorizationStatus()
        capabilities = currentCapabilities()
    }

    func requestPermission(for type: PermissionType) async {
        switch type {
        case .location:
            _ = await locationService.requestAuthorization()
        case .health:
            _ = await healthService.requestAuthorization()
        case .notifications:
            let status = await notificationService.requestAuthorization()
            if let idx = capabilities.firstIndex(where: { $0.permissionType == .notifications }) {
                capabilities[idx].status = status
            }
            if status == .authorized {
                NotificationCenter.default.post(name: .heraldPushPermissionGranted, object: nil)
            }
        case .microphone:
            await requestMicrophoneAuthorization()
        case .camera:
            _ = await mediaService.requestCameraAuthorization()
        case .photos:
            _ = await mediaService.requestPhotosAuthorization()
        case .motion:
            _ = await motionService?.requestAuthorization()
        case .speechRecognition:
            await requestSpeechAuthorization()
        }

        capabilities = currentCapabilities()
    }

    var locationAuthorizationLevel: LocationAuthorizationLevel {
        locationService.authorizationLevel
    }

    var locationAccuracyLevel: LocationAccuracyLevel {
        locationService.accuracyLevel
    }

    var healthBackgroundDeliveryEnabled: Bool {
        healthService.backgroundDeliveryEnabled
    }

    func requestBackgroundLocationAccess() async {
        _ = await locationService.requestBackgroundAuthorization()
        capabilities = currentCapabilities()
    }

    func updateLocationSyncPreference(_ preference: LocationSyncPreference) {
        locationService.updateSyncPreference(preference)
        capabilities = currentCapabilities()
    }

    func openLocationSystemSettings() {
        locationService.openSystemSettings()
    }

    private func currentCapabilities() -> [DeviceCapability] {
        [
            DeviceCapability(
                permissionType: .location,
                status: locationService.authorizationStatus,
                statusDetail: locationStatusDetail()
            ),
            DeviceCapability(
                permissionType: .health,
                status: healthService.authorizationStatus,
                statusDetail: healthStatusDetail()
            ),
            DeviceCapability(permissionType: .notifications, status: notificationService.authorizationStatus),
            DeviceCapability(permissionType: .microphone, status: microphoneAuthorizationStatus()),
            DeviceCapability(permissionType: .camera, status: mediaService.cameraAuthorizationStatus),
            DeviceCapability(permissionType: .photos, status: mediaService.photosAuthorizationStatus),
            DeviceCapability(permissionType: .motion, status: motionService?.authorizationStatus ?? .unsupported),
            DeviceCapability(
                permissionType: .speechRecognition,
                status: speechRecognitionStatus(),
                statusDetail: Self.speechRecognitionStatusDetail(for: speechRecognitionStatus())
            ),
        ]
    }

    // MARK: - Microphone

    private func microphoneAuthorizationStatus() -> PermissionStatus {
        switch AVAudioApplication.shared.recordPermission {
        case .granted: .authorized
        case .denied: .denied
        case .undetermined: .notDetermined
        @unknown default: .notDetermined
        }
    }

    private func requestMicrophoneAuthorization() async {
        guard AVAudioApplication.shared.recordPermission == .undetermined else { return }
        await withCheckedContinuation { continuation in
            AVAudioApplication.requestRecordPermission { _ in
                continuation.resume()
            }
        }
    }

    // MARK: - Speech Recognition

    /// Returns the availability-aware speech recognition status.
    /// SFSpeechRecognizer.authorizationStatus() works on all iOS versions
    /// since iOS 10. The deployment target is iOS 18.
    static func speechRecognitionAvailabilityStatus() -> PermissionStatus {
        let status = SFSpeechRecognizer.authorizationStatus()
        switch status {
        case .authorized: return .authorized
        case .denied: return .denied
        case .restricted: return .restricted
        case .notDetermined: return .notDetermined
        @unknown default: return .notDetermined
        }
    }

    /// Returns a user-facing status detail for speech recognition.
    static func speechRecognitionStatusDetail(for status: PermissionStatus) -> String? {
        switch status {
        case .restricted:
            return "Speech recognition is restricted on this device"
        default:
            return nil
        }
    }

    private func speechRecognitionStatus() -> PermissionStatus {
        Self.speechRecognitionAvailabilityStatus()
    }

    private func requestSpeechAuthorization() async {
        if #available(iOS 26.0, *) {
            // iOS 26+: SFSpeechRecognizer.requestAuthorization crashes on beta
            // (FB-prefixed radar). The modern SpeechAnalyzer/DictationTranscriber
            // APIs handle authorization automatically when first used — the
            // system presents the TCC dialog inline. Trigger that initialization
            // via the provided closure so the dialog actually appears.
            guard SFSpeechRecognizer.authorizationStatus() == .notDetermined else { return }
            if let trigger = speechAuthorizationTrigger {
                await trigger()
            }
        } else {
            // iOS 18-25: SFSpeechRecognizer.requestAuthorization() works correctly
            guard SFSpeechRecognizer.authorizationStatus() == .notDetermined else { return }
            await withCheckedContinuation { continuation in
                SFSpeechRecognizer.requestAuthorization { status in
                    continuation.resume()
                }
            }
        }
    }

    private func locationStatusDetail() -> String? {
        switch locationService.authorizationLevel {
        case .whenInUse, .always:
            return "\(locationService.authorizationLevel.displayLabel) • \(locationService.accuracyLevel.displayLabel)"
        case .notDetermined, .denied, .restricted:
            return nil
        }
    }

    private func healthStatusDetail() -> String? {
        switch healthService.authorizationStatus {
        case .authorized:
            let backgroundStatus = healthService.backgroundDeliveryEnabled ? "Background Sync On" : "Background Sync Off"
            return "Read Only • \(backgroundStatus)"
        case .unsupported:
            return "Health data is not available in this build"
        case .denied, .restricted:
            return "Manage in Apple Health or Settings > Privacy & Security > Health"
        default:
            return nil
        }
    }
}

extension Notification.Name {
    /// Posted when the user grants notification permission so that
    /// AppContainer can trigger APNs token registration immediately.
    static let heraldPushPermissionGranted = Notification.Name("HeraldPushPermissionGranted")
}
