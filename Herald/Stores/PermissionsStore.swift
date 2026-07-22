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
            _ = await notificationService.requestAuthorization()
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
    /// On iOS < 26, returns `.unsupported` since the modern Speech APIs
    /// (SpeechAnalyzer, DictationTranscriber) are not available.
    static func speechRecognitionAvailabilityStatus() -> PermissionStatus {
        if #available(iOS 26.0, *) {
            switch SFSpeechRecognizer.authorizationStatus() {
            case .authorized: return .authorized
            case .denied: return .denied
            case .restricted: return .restricted
            case .notDetermined: return .notDetermined
            @unknown default: return .notDetermined
            }
        } else {
            return .unsupported
        }
    }

    /// Returns a user-facing status detail for speech recognition.
    /// On iOS < 26, explains that the feature requires a newer OS.
    static func speechRecognitionStatusDetail(for status: PermissionStatus) -> String? {
        switch status {
        case .unsupported:
            return "Requires iOS 26 or later"
        default:
            return nil
        }
    }

    private func speechRecognitionStatus() -> PermissionStatus {
        Self.speechRecognitionAvailabilityStatus()
    }

    private func requestSpeechAuthorization() async {
        guard #available(iOS 26.0, *) else { return }
        guard SFSpeechRecognizer.authorizationStatus() == .notDetermined else { return }
        await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { _ in
                // Resume on main actor to avoid thread-safety issues that can
                // cause crashes on iOS 18 when the TCC dialog dismisses.
                Task { @MainActor in
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
