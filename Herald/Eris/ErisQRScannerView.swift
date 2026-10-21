import AVFoundation
import SwiftUI
import UIKit

// MARK: - QR scanner
//
// AVFoundation behind a `UIViewControllerRepresentable`, not VisionKit, so the
// scanner works without the data-scanner hardware check. Every failure path ends
// in `onUnavailable` instead of a trap: a simulator has no capture device, a
// denied camera cannot be opened, and neither may crash the pairing sheet. The
// caller's job is to keep manual entry on screen.

/// A camera QR scanner for the Eris pairing payload.
struct ErisQRScannerView: UIViewControllerRepresentable {
    /// Called once with the first decoded payload. Main-actor: the sheet reacts.
    let onPayload: @MainActor (String) -> Void
    /// Called when the camera cannot be used, with a reason for a human.
    let onUnavailable: @MainActor (String) -> Void

    func makeUIViewController(context: Context) -> ErisQRScannerController {
        ErisQRScannerController(onPayload: onPayload, onUnavailable: onUnavailable)
    }

    func updateUIViewController(_ uiViewController: ErisQRScannerController, context: Context) {}
}

/// The capture controller: an `AVCaptureSession` with a QR metadata output.
@MainActor
final class ErisQRScannerController: UIViewController {
    private let onPayload: @MainActor (String) -> Void
    private let onUnavailable: @MainActor (String) -> Void

    private let session = AVCaptureSession()
    private let captureQueue = DispatchQueue(label: "kallisti.eris.qr", qos: .userInitiated)
    private var metadataOutput: AVCaptureMetadataOutput?
    private var previewLayer: AVCaptureVideoPreviewLayer?
    /// Set once a payload has been handed over, so one code cannot fire twice.
    private var hasDeliveredPayload = false
    /// Whether the session graph has been built, so it is built exactly once.
    private var isConfigured = false
    /// Set when the camera turned out to be unusable, so start is never retried.
    private var isUnusable = false

    init(
        onPayload: @escaping @MainActor (String) -> Void,
        onUnavailable: @escaping @MainActor (String) -> Void
    ) {
        self.onPayload = onPayload
        self.onUnavailable = onUnavailable
        super.init(nibName: nil, bundle: nil)
        modalPresentationStyle = .fullScreen
    }

    /// Never used: this controller is only ever created in code. It answers
    /// `nil` rather than trapping, so an unexpected unarchive cannot crash.
    required init?(coder: NSCoder) {
        self.onPayload = { _ in }
        self.onUnavailable = { _ in }
        super.init(coder: coder)
        return nil
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        requestCameraAccessThenConfigure()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        stopSession()
    }

    // MARK: Setup

    /// Ask for the camera, then build the graph only if the answer is yes.
    private func requestCameraAccessThenConfigure() {
        guard !isConfigured, !isUnusable else { return }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            configureSession()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                Task { @MainActor in
                    guard let self else { return }
                    if granted {
                        self.configureSession()
                    } else {
                        self.reportUnavailable(
                            "Camera access is off for Kallisti. Enter the daemon address and code instead."
                        )
                    }
                }
            }
        case .denied, .restricted:
            reportUnavailable(
                "Camera access is off for Kallisti. Enter the daemon address and code instead."
            )
        @unknown default:
            reportUnavailable("The camera could not be used on this device.")
        }
    }

    private func configureSession() {
        guard !isConfigured, !isUnusable else { return }

        guard let device = AVCaptureDevice.default(for: .video) else {
            // A simulator has no capture device at all.
            reportUnavailable("This device has no camera. Enter the daemon address and code instead.")
            return
        }
        guard let input = try? AVCaptureDeviceInput(device: device) else {
            reportUnavailable("The camera could not be opened. Enter the daemon address and code instead.")
            return
        }

        session.beginConfiguration()
        defer { session.commitConfiguration() }

        guard session.canAddInput(input) else {
            reportUnavailable("The camera could not be opened. Enter the daemon address and code instead.")
            return
        }
        session.addInput(input)

        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            reportUnavailable("The camera cannot read codes on this device. Enter the daemon address and code instead.")
            return
        }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        // Only ask for the symbology this surface can actually read, and only
        // when the device offers it: assigning an unsupported type traps.
        if output.availableMetadataObjectTypes.contains(.qr) {
            output.metadataObjectTypes = [.qr]
        } else {
            reportUnavailable("This camera cannot read QR codes. Enter the daemon address and code instead.")
            return
        }
        metadataOutput = output
        isConfigured = true

        let preview = AVCaptureVideoPreviewLayer(session: session)
        preview.videoGravity = .resizeAspectFill
        preview.frame = view.bounds
        view.layer.addSublayer(preview)
        previewLayer = preview
    }

    // MARK: Running

    private func startSession() {
        guard isConfigured, !session.isRunning else { return }
        // `startRunning` blocks; the project's camera manager runs it off-main
        // for the same reason.
        let capturedSession = session
        captureQueue.async {
            capturedSession.startRunning()
        }
    }

    private func stopSession() {
        guard session.isRunning else { return }
        let capturedSession = session
        captureQueue.async {
            capturedSession.stopRunning()
        }
    }

    /// Hand the reason to the caller once and stop touching the camera.
    private func reportUnavailable(_ reason: String) {
        guard !isUnusable else { return }
        isUnusable = true
        stopSession()
        onUnavailable(reason)
    }
}

// MARK: - Metadata delegate

extension ErisQRScannerController: AVCaptureMetadataOutputObjectsDelegate {
    nonisolated func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        // Only the payload string crosses the actor boundary; the metadata
        // object itself is not `Sendable`.
        var payload: String?
        for object in metadataObjects {
            guard
                let code = object as? AVMetadataMachineReadableCodeObject,
                code.type == .qr,
                let value = code.stringValue,
                !value.isEmpty
            else { continue }
            payload = value
            break
        }
        guard let payload else { return }
        Task { @MainActor [weak self] in
            guard let self, !self.hasDeliveredPayload else { return }
            self.hasDeliveredPayload = true
            self.stopSession()
            self.onPayload(payload)
        }
    }
}
