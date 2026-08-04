import AVFoundation
import Foundation
import Speech

/// iOS 18–25 speech recognition using the classic SFSpeechRecognizer API.
///
/// On iOS 26+, the modern ``LiveSpeechService`` (DictationTranscriber /
/// SpeechAnalyzer) is preferred. This fallback uses the SFSpeechRecognizer
/// + SFSpeechAudioBufferRecognitionRequest path that has been available
/// since iOS 10 and does not crash on any released OS version.
@MainActor
final class LegacySpeechService: SpeechDictationService {
    private let speechRecognizer = SFSpeechRecognizer()
    private let audioEngine = AVAudioEngine()
    private var recognitionRequest: SFSpeechAudioBufferRecognitionRequest?
    private var recognitionTask: SFSpeechRecognitionTask?

    private(set) var isListening = false
    private(set) var transcript = ""

    var onAutoStop: ((String) -> Void)?
    var onTranscriptChange: ((String) -> Void)?

    deinit {
        // Don't call stopListening() from deinit — it's @MainActor isolated.
        // The audio engine is torn down automatically when the instance is
        // deallocated.
    }

    func requestAuthorization() async -> SFSpeechRecognizerAuthorizationStatus {
        await SpeechAuthorizationBridge.requestAuthorization()
    }

    func startListening() async throws {
        guard !isListening else { return }
        guard let recognizer = speechRecognizer, recognizer.isAvailable else {
            throw SpeechServiceError.unavailable
        }

        // Stop any previous session
        stopListening()

        let audioSession = AVAudioSession.sharedInstance()
        try audioSession.setCategory(.record, mode: .measurement, options: .duckOthers)
        try audioSession.setActive(true, options: .notifyOthersOnDeactivation)

        recognitionRequest = SFSpeechAudioBufferRecognitionRequest()
        guard let recognitionRequest else {
            throw SpeechServiceError.failedToCreateRequest
        }
        recognitionRequest.shouldReportPartialResults = true

        recognitionTask = recognizer.recognitionTask(with: recognitionRequest) { [weak self] result, error in
            guard let self else { return }
            if let result {
                let newTranscript = result.bestTranscription.formattedString
                self.transcript = newTranscript
                self.onTranscriptChange?(newTranscript)
            }
            if error != nil || result?.isFinal == true {
                self.stopListening()
                if let final = result?.bestTranscription.formattedString, !final.isEmpty {
                    self.onAutoStop?(final)
                }
            }
        }

        let inputNode = audioEngine.inputNode
        let recordingFormat = inputNode.outputFormat(forBus: 0)
        inputNode.installTap(onBus: 0, bufferSize: 1024, format: recordingFormat) { buffer, _ in
            recognitionRequest.append(buffer)
        }

        audioEngine.prepare()
        try audioEngine.start()
        isListening = true
    }

    func stopListening() {
        guard isListening else { return }
        audioEngine.stop()
        audioEngine.inputNode.removeTap(onBus: 0)
        recognitionRequest?.endAudio()
        recognitionTask?.cancel()
        recognitionRequest = nil
        recognitionTask = nil
        isListening = false
    }

    enum SpeechServiceError: Error, LocalizedError {
        case unavailable
        case failedToCreateRequest

        var errorDescription: String? {
            switch self {
            case .unavailable: "Speech recognition is not available"
            case .failedToCreateRequest: "Failed to create recognition request"
            }
        }
    }
}
