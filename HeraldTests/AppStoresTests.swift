import Foundation
import HealthKit
import Testing
import UIKit
@testable import Herald

@Suite(.serialized)
struct AppStoresTests {

    private final class StubURLProtocol: URLProtocol, @unchecked Sendable {
        nonisolated(unsafe) static var requestHandler: (@Sendable (URLRequest) throws -> (HTTPURLResponse, Data))?

        override class func canInit(with request: URLRequest) -> Bool {
            true
        }

        override class func canonicalRequest(for request: URLRequest) -> URLRequest {
            request
        }

        override func startLoading() {
            guard let handler = Self.requestHandler else {
                client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
                return
            }

            do {
                let (response, data) = try handler(request)
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: data)
                client?.urlProtocolDidFinishLoading(self)
            } catch {
                client?.urlProtocol(self, didFailWithError: error)
            }
        }

        override func stopLoading() {}
    }

    private final class MutableBox<T>: @unchecked Sendable {
        var value: T

        init(_ value: T) {
            self.value = value
        }
    }

    private struct TimestampPayload: Decodable {
        let timestamp: Date
    }

    private struct FakeBundleInfo {
        static let hostedRelayURL = "https://managed.example.com/v1"
        static let pushBrokerURL = "https://broker.example.com/v1"
    }

    private func makeSetupCode(_ code: String = "ABCD-EFGH") -> String {
        code
    }

    private func requestBodyString(_ request: URLRequest) -> String {
        if let body = request.httpBody, let string = String(data: body, encoding: .utf8) {
            return string
        }
        guard let stream = request.httpBodyStream else { return "" }
        stream.open()
        defer { stream.close() }
        let bufferSize = 4096
        let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: bufferSize)
        defer { buffer.deallocate() }
        var data = Data()
        while stream.hasBytesAvailable {
            let count = stream.read(buffer, maxLength: bufferSize)
            if count <= 0 { break }
            data.append(buffer, count: count)
        }
        return String(data: data, encoding: .utf8) ?? ""
    }

    @MainActor
    private final class RecordingSessionBootstrapService: SessionBootstrapServiceProtocol {
        var registerCallCount = 0
        var lastLoadedAccessToken: String?

        func registerDevice(_ request: DeviceRegistrationRequest) async throws -> SessionBootstrapResponse {
            registerCallCount += 1
            return SessionBootstrapResponse(
                state: AppSessionState(
                    deviceID: UUID(),
                    installationID: request.installationID,
                    deviceRegistered: true,
                    connectionStatus: .connected,
                    syncStatus: .synced,
                    isMockMode: false,
                    backendEndpoint: request.relayBaseURLString,
                    lastSyncAt: nil,
                    pushTokenRegistered: false
                ),
                tokens: AuthTokens(
                    accessToken: "recording-access-token",
                    refreshToken: "recording-refresh-token",
                    expiresAt: .distantFuture
                )
            )
        }

        func loadSession(accessToken: String?) async throws -> AppSessionState {
            lastLoadedAccessToken = accessToken
            return AppSessionState(
                userID: UUID(),
                displayName: "Herald User",
                deviceID: UUID(),
                installationID: UUID(),
                deviceRegistered: true,
                connectionStatus: .connected,
                syncStatus: .synced,
                isMockMode: false,
                backendEndpoint: AppEnvironment.development.baseURLString,
                lastSyncAt: .now,
                pushTokenRegistered: false
            )
        }

        func refreshAuth(refreshToken: String) async throws -> AuthTokens {
            AuthTokens(
                accessToken: "refreshed-access-token",
                refreshToken: "refreshed-refresh-token",
                expiresAt: .distantFuture
            )
        }

        func revokeCurrentSession(accessToken: String?) async throws {}
    }

    @MainActor
    private final class RecordingPairingService: PairingServiceProtocol {
        func normalizePairingCode(_ rawCode: String) throws -> String {
            try PhonePairingCode.normalize(rawCode)
        }

        func redeemPairingCode(
            _ normalizedCode: String,
            request: DeviceRegistrationRequest
        ) async throws -> PairingRedeemResult {
            PairingRedeemResult(
                configuration: PairedRelayConfiguration(
                    baseURLString: request.relayBaseURLString,
                    hostDisplayName: URL(string: request.relayBaseURLString)?.host ?? request.relayBaseURLString,
                    pairedAt: .now
                ),
                state: AppSessionState(
                    userID: UUID(),
                    displayName: "Morgan",
                    deviceID: UUID(),
                    installationID: request.installationID,
                    deviceRegistered: true,
                    connectionStatus: .connected,
                    syncStatus: .synced,
                    isMockMode: false,
                    backendEndpoint: request.relayBaseURLString,
                    lastSyncAt: .now,
                    pushTokenRegistered: false
                ),
                tokens: AuthTokens(
                    accessToken: "paired-access-token-\(normalizedCode)",
                    refreshToken: "paired-refresh-token-\(normalizedCode)",
                    expiresAt: .distantFuture
                )
            )
        }
    }

    @MainActor
    private final class RecordingHeraldHostService: HeraldHostServiceProtocol {
        var currentHost: HeraldHostStatus?
        var fetchError: Error?

        func fetchCurrentHost(accessToken: String?) async throws -> HeraldHostStatus? {
            if let fetchError {
                throw fetchError
            }
            return currentHost
        }

        func createEnrollmentCode(accessToken: String?) async throws -> HostEnrollmentCode {
            HostEnrollmentCode(
                setupCode: "HC1:test-setup-code",
                expiresAt: .distantFuture,
                relayHost: "relay.example.test"
            )
        }

        func revokeCurrentHost(accessToken: String?) async throws {
            currentHost = nil
        }
    }

    @MainActor
    private final class RecordingHeraldClient: HeraldClientProtocol {
        var connectionStatus: ConnectionStatus = .connected
        var currentConversation: Conversation?
        var sendCallCount = 0
        var lastClientMessageID: UUID?
        var nextResponse = Message(sender: .herald, content: "Recorded response", status: .delivered)

        func connect() async {}

        func disconnect() async {}

        func send(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) async -> Message {
            sendCallCount += 1
            lastClientMessageID = clientMessageID
            return nextResponse
        }

        func sendStreaming(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
            AsyncStream { continuation in
                Task { @MainActor in
                    sendCallCount += 1
                    lastClientMessageID = clientMessageID
                    continuation.yield(.messageSent(jobID: UUID()))
                    continuation.yield(.finished(nextResponse, nil, nil, nil))
                    continuation.finish()
                }
            }
        }

        func loadConversation() async -> Conversation {
            currentConversation ?? Conversation(title: "Herald")
        }

        func clearConversation() async throws -> Conversation {
            let conversation = Conversation(title: "Herald")
            currentConversation = conversation
            return conversation
        }

        func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
            currentConversation ?? Conversation(title: "Herald")
        }

        func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse {
            SessionListResponse(sessions: [], total: 0)
        }

        func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] {
            []
        }

        func createSession(title: String) async throws -> SessionSummary {
            SessionSummary(id: UUID(), title: title)
        }

        func deleteSession(id: UUID) async throws {}

        func archiveSession(id: UUID) async throws {}

        func togglePinSession(id: UUID) async throws -> SessionSummary {
            SessionSummary(id: id, title: "Test")
        }

        func renameSession(id: UUID, title: String) async throws -> SessionSummary {
            SessionSummary(id: id, title: title)
        }

        func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
            "New Chat"
        }

        func loadConversation(id: UUID) async throws -> Conversation {
            currentConversation ?? Conversation(title: "Herald")
        }

        func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? {
            nil
        }

        func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
            nextResponse
        }

        func cancelJob(jobID: UUID) async throws {}
    }

    private struct FakeAppAttestProof: Sendable {
        let keyId: String
        let attestationObject: String
        let assertion: String
    }

    @MainActor
    private final class FakeAppAttestService: AppAttestServiceProtocol {
        var callCount = 0
        var lastChallenge: String?
        var lastSignedPayload: Data?
        var proof = FakeAppAttestProof(
            keyId: "attest-key",
            attestationObject: "attestation-proof",
            assertion: "assertion-proof"
        )

        func createProof(challenge: String, signedPayload: Data) async throws -> AppAttestProof {
            callCount += 1
            lastChallenge = challenge
            lastSignedPayload = signedPayload
            return AppAttestProof(
                keyId: proof.keyId,
                attestationObject: proof.attestationObject,
                assertion: proof.assertion
            )
        }
    }

    @Test @MainActor
    func sessionBootstrapPersistsStateAndTokens() async throws {
        let suiteName = "session-bootstrap-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        let sessionStore = AppSessionStore(
            bootstrapService: MockSessionBootstrapService(),
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )

        await sessionStore.bootstrap()

        #expect(sessionStore.state.deviceRegistered)
        #expect(sessionStore.state.connectionStatus == .connected)
        #expect(await secureStore.retrieve(key: "session.accessToken") != nil)
        #expect(persistence.loadSessionState()?.deviceRegistered == true)
    }

    @Test @MainActor
    func sessionBootstrapReRegistersWhenPersistedStateExistsButAccessTokenIsMissing() async throws {
        let suiteName = "session-bootstrap-missing-token-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        persistence.saveSessionState(
            AppSessionState(
                userID: UUID(),
                displayName: "Herald User",
                deviceID: UUID(),
                installationID: UUID(),
                deviceRegistered: true,
                connectionStatus: .connected,
                syncStatus: .synced,
                isMockMode: false,
                backendEndpoint: AppEnvironment.development.baseURLString,
                lastSyncAt: .now,
                pushTokenRegistered: false
            )
        )

        let bootstrapService = RecordingSessionBootstrapService()
        let secureStore = MockSecureStore()
        let sessionStore = AppSessionStore(
            bootstrapService: bootstrapService,
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )

        await sessionStore.bootstrap()

        #expect(bootstrapService.registerCallCount == 1)
        #expect(bootstrapService.lastLoadedAccessToken == "recording-access-token")
        #expect(await secureStore.retrieve(key: "session.accessToken") == "recording-access-token")
    }

    @Test @MainActor
    func settingsStorePersistsEnvironmentChanges() async throws {
        let suiteName = "settings-store-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let settingsStore = SettingsStore(persistence: persistence)

        settingsStore.settings.environment = .staging

        let reloaded = persistence.loadUserSettings()
        #expect(reloaded?.environment == .staging)
    }

    @Test @MainActor
    func settingsStorePersistsLocationSyncPreferenceChanges() async throws {
        let suiteName = "settings-store-location-sync-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let settingsStore = SettingsStore(persistence: persistence)

        settingsStore.settings.locationSyncPreference = .backgroundAllowed

        let reloaded = persistence.loadUserSettings()
        #expect(reloaded?.locationSyncPreference == .backgroundAllowed)
    }

    @Test @MainActor
    func sleepDurationUsesStableWakeDayBucket() async throws {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!

        let bucketDay = calendar.date(from: DateComponents(year: 2026, month: 4, day: 5))!
        let intervals: [LiveHealthService.SleepInterval] = [
            .init(
                value: HKCategoryValueSleepAnalysis.asleepCore.rawValue,
                startDate: calendar.date(from: DateComponents(year: 2026, month: 4, day: 4, hour: 23, minute: 0))!,
                endDate: calendar.date(from: DateComponents(year: 2026, month: 4, day: 5, hour: 7, minute: 0))!
            ),
            .init(
                value: HKCategoryValueSleepAnalysis.asleepREM.rawValue,
                startDate: calendar.date(from: DateComponents(year: 2026, month: 4, day: 5, hour: 13, minute: 0))!,
                endDate: calendar.date(from: DateComponents(year: 2026, month: 4, day: 5, hour: 13, minute: 30))!
            ),
            .init(
                value: HKCategoryValueSleepAnalysis.asleepDeep.rawValue,
                startDate: calendar.date(from: DateComponents(year: 2026, month: 4, day: 5, hour: 23, minute: 0))!,
                endDate: calendar.date(from: DateComponents(year: 2026, month: 4, day: 6, hour: 6, minute: 0))!
            ),
        ]

        let hours = LiveHealthService.aggregateSleepDuration(
            intervals: intervals,
            attributedTo: bucketDay,
            calendar: calendar
        )

        #expect(hours == 8.5)
        #expect(LiveHealthService.sleepBucketDay(for: calendar.date(from: DateComponents(year: 2026, month: 4, day: 5, hour: 18))!, calendar: calendar) == bucketDay)
    }

    @Test @MainActor
    func chatStorePassesClientMessageIDAndSkipsPendingDuplicate() async throws {
        let heraldClient = RecordingHeraldClient()
        let suiteName = "chat-store-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        await chatStore.sendMessage("Hello Herald")

        #expect(heraldClient.sendCallCount == 1)
        #expect(heraldClient.lastClientMessageID != nil)

        chatStore.conversation = Conversation(
            title: "Herald",
            messages: [
                Message(sender: .user, content: "Still waiting", status: .sending),
            ]
        )

        await chatStore.sendMessage("Still waiting")

        #expect(heraldClient.sendCallCount == 1)
        #expect(chatStore.conversation?.messages.count == 1)
    }

    @Test @MainActor
    func chatStorePreservesStreamingArtifactsAfterConversationRefresh() async throws {
        final class StreamingArtifactClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?

            func connect() async {}
            func disconnect() async {}

            func send(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "unused", status: .delivered)
            }

            func sendStreaming(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                let jobID = UUID()
                let finalMessageID = UUID()
                currentConversation = Conversation(
                    title: "Herald",
                    messages: [
                        Message(id: clientMessageID, sender: .user, content: message, status: .sent),
                        Message(id: finalMessageID, sender: .herald, content: "Patched answer", jobID: jobID, status: .delivered),
                    ]
                )

                let diff = CodeDiff(
                    files: [
                        FileDiff(
                            path: "src/example.py",
                            status: "modified",
                            additions: 2,
                            deletions: 1,
                            patch: "@@ -1 +1 @@\n-old\n+new"
                        ),
                    ],
                    summary: "1 file changed, 2 insertions(+), 1 deletion(-)"
                )

                return AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: jobID))
                        continuation.yield(.toolActivity("🔍 Searching files"))
                        continuation.yield(.finished(
                            Message(
                                id: finalMessageID,
                                sender: .herald,
                                content: "Patched answer",
                                jobID: jobID,
                                status: .delivered
                            ),
                            nil,
                            diff,
                            nil
                        ))
                        continuation.finish()
                    }
                }
            }

            func loadConversation() async -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func clearConversation() async throws -> Conversation {
                let conversation = Conversation(title: "Herald")
                currentConversation = conversation
                return conversation
            }

            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let suiteName = "chat-store-stream-artifacts-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let heraldClient = StreamingArtifactClient()
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        await chatStore.sendMessage("Fix the bug")

        let heraldMessage = chatStore.conversation?.messages.last(where: { $0.sender == .herald })
        #expect(heraldMessage?.toolActivities.count == 1)
        #expect(heraldMessage?.codeDiff?.fileCount == 1)
        #expect(heraldMessage?.codeDiff?.summary == "1 file changed, 2 insertions(+), 1 deletion(-)")
    }

    @Test @MainActor
    func chatStorePreservesStreamingPlaceholderDuringConversationRefresh() async throws {
        final class PlaceholderRefreshClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?

            func connect() async {}
            func disconnect() async {}

            func send(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "unused", status: .delivered)
            }

            func sendStreaming(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.finish()
                    }
                }
            }

            func loadConversation() async -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func clearConversation() async throws -> Conversation {
                Conversation(title: "Herald")
            }

            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let suiteName = "chat-store-placeholder-refresh-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let heraldClient = PlaceholderRefreshClient()
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        let userMessage = Message(sender: .user, content: "Waiting", status: .sending)
        let placeholder = Message(sender: .herald, content: "", status: .sending, isStreaming: true)
        chatStore.conversation = Conversation(title: "Herald", messages: [userMessage, placeholder])
        heraldClient.currentConversation = Conversation(title: "Herald", messages: [userMessage])

        await chatStore.loadConversation()

        #expect(chatStore.conversation?.messages.count == 2)
        #expect(chatStore.conversation?.messages.last?.isStreaming == true)
    }

    @Test @MainActor
    func chatStoreKeepsAcceptedMessagePendingUntilTerminalResultArrives() async throws {
        final class PendingUntilFinishedClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?

            func connect() async {}
            func disconnect() async {}

            func send(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "unused", status: .delivered)
            }

            func sendStreaming(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: UUID()))
                        try? await Task.sleep(for: .milliseconds(50))
                        continuation.yield(.finished(Message(sender: .herald, content: "Done", status: .delivered), nil, nil, nil))
                        continuation.finish()
                    }
                }
            }

            func loadConversation() async -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func clearConversation() async throws -> Conversation {
                Conversation(title: "Herald")
            }

            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let suiteName = "chat-store-pending-until-finished-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let heraldClient = PendingUntilFinishedClient()
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        let task = Task { await chatStore.sendMessage("Hello") }
        try? await Task.sleep(for: .milliseconds(10))

        let userMessage = try #require(chatStore.conversation?.messages.first(where: { $0.sender == .user }))
        #expect(userMessage.status == .sending)

        await task.value
    }

    @Test @MainActor
    func chatStoreRefreshesConversationWhenStreamingFailsAfterJobAccepted() async throws {
        final class StreamingFailureClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?
            var loadConversationCallCount = 0
            let jobID = UUID()
            let userID = UUID()
            let assistantID = UUID()

            func connect() async {}
            func disconnect() async {}

            func send(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "unused", status: .delivered)
            }

            func sendStreaming(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                currentConversation = Conversation(
                    title: "Herald",
                    messages: [
                        Message(id: userID, clientMessageID: clientMessageID, sender: .user, content: message, status: .sent),
                    ]
                )

                return AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: jobID))
                        continuation.yield(.failed("Stream interrupted"))
                        continuation.finish()
                    }
                }
            }

            func loadConversation() async -> Conversation {
                loadConversationCallCount += 1
                let conversation = Conversation(
                    title: "Herald",
                    messages: [
                        Message(id: userID, sender: .user, content: "Fix it", status: .delivered),
                        Message(id: assistantID, sender: .herald, content: "Recovered after polling", jobID: jobID, status: .delivered),
                    ]
                )
                currentConversation = conversation
                return conversation
            }

            func clearConversation() async throws -> Conversation {
                Conversation(title: "Herald")
            }

            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation { await loadConversation() }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let suiteName = "chat-store-stream-failure-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let heraldClient = StreamingFailureClient()
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        await chatStore.sendMessage("Fix it")

        #expect(heraldClient.loadConversationCallCount == 1)
        #expect(chatStore.conversation?.messages.last?.content == "Recovered after polling")
        #expect(chatStore.pendingMessageSentAt == nil)
        #expect(chatStore.isStreaming == false)
    }

    @Test @MainActor
    func liveHeraldClientRefreshesConversationBeforeResolvingFinishedStreamMessage() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: configuration)

        let conversationID = UUID()
        let userMessageID = UUID()
        let assistantMessageID = UUID()
        let jobID = UUID()
        let requestCount = MutableBox(0)

        StubURLProtocol.requestHandler = { request in
            requestCount.value += 1
            let url = try #require(request.url)

            switch url.absoluteString {
            case "https://relay.example.com/v1/messages":
                let response = HTTPURLResponse(url: url, statusCode: 202, httpVersion: nil, headerFields: nil)!
                let data = #"""
                {"data":{
                  "replyState":"pending",
                  "jobId":"\#(jobID.uuidString.lowercased())",
                  "conversation":{
                    "id":"\#(conversationID.uuidString)",
                    "title":"Herald",
                    "updatedAt":"2026-04-05T18:00:00Z",
                    "messages":[
                      {
                        "id":"\#(userMessageID.uuidString)",
                        "clientMessageId":"\#(userMessageID.uuidString)",
                        "role":"user",
                        "text":"Look at this",
                        "timestamp":"2026-04-05T18:00:00Z",
                        "deliveryStatus":"sent"
                      }
                    ]
                  },
                  "userMessage":{
                    "id":"\#(userMessageID.uuidString)",
                    "clientMessageId":"\#(userMessageID.uuidString)",
                    "role":"user",
                    "text":"Look at this",
                    "timestamp":"2026-04-05T18:00:00Z",
                    "deliveryStatus":"sent",
                    "jobId":"\#(jobID.uuidString.lowercased())"
                  }
                }}
                """#.data(using: .utf8)!
                return (response, data)

            case "https://relay.example.com/v1/jobs/\(jobID.uuidString.lowercased())/events":
                let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: ["Content-Type": "text/event-stream"])!
                let data = """
                event: text_delta
                data: {"jobId":"\(jobID.uuidString.lowercased())","delta":"Recovered ","kind":"text_delta"}

                event: done
                data: {"jobId":"\(jobID.uuidString.lowercased())","status":"completed"}

                """.data(using: .utf8)!
                return (response, data)

            case "https://relay.example.com/v1/jobs/\(jobID.uuidString.lowercased())":
                let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
                let data = #"{"data":{"jobId":"\#(jobID.uuidString.lowercased())","status":"completed","conversationId":"\#(conversationID.uuidString)","attempt":0,"lastSeq":0}}"#.data(using: .utf8)!
                return (response, data)

            case "https://relay.example.com/v1/conversations/current":
                let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
                let data = #"""
                {"data":{
                  "conversation":{
                    "id":"\#(conversationID.uuidString)",
                    "title":"Herald",
                    "updatedAt":"2026-04-05T18:00:01Z",
                    "messages":[
                      {
                        "id":"\#(userMessageID.uuidString)",
                        "clientMessageId":"\#(userMessageID.uuidString)",
                        "role":"user",
                        "text":"Look at this",
                        "timestamp":"2026-04-05T18:00:00Z",
                        "deliveryStatus":"delivered",
                        "jobId":"\#(jobID.uuidString.lowercased())"
                      },
                      {
                        "id":"\#(assistantMessageID.uuidString)",
                        "role":"herald",
                        "text":"Recovered after refresh",
                        "timestamp":"2026-04-05T18:00:01Z",
                        "deliveryStatus":"delivered",
                        "jobId":"\#(jobID.uuidString.lowercased())"
                      }
                    ]
                  }
                }}
                """#.data(using: .utf8)!
                return (response, data)

            default:
                Issue.record("Unexpected URL: \(url.absoluteString)")
                throw URLError(.badURL)
            }
        }

        defer {
            StubURLProtocol.requestHandler = nil
        }

        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: session
        )
        let heraldClient = LiveHeraldClient(
            apiClient: apiClient,
            accessTokenProvider: { "token" },
            allowDemoFallback: false
        )

        var updates: [StreamingUpdate] = []
        for await update in heraldClient.sendStreaming(
            message: "Look at this",
            attachments: [],
            clientMessageID: userMessageID
        ) {
            updates.append(update)
        }

        let finishedMessage = try #require(
            updates.compactMap { update -> Message? in
                guard case .finished(let message, _, _, _) = update else { return nil }
                return message
            }.last
        )
        #expect(finishedMessage.content == "Recovered after refresh")
        #expect(requestCount.value == 4)
    }

    @Test @MainActor
    func liveHeraldClientRejectsOversizedAggregateAttachmentPayloadBeforeSending() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: configuration)
        let requestCount = MutableBox(0)

        StubURLProtocol.requestHandler = { request in
            requestCount.value += 1
            let url = try #require(request.url)
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
            return (response, #"{"data":{"conversation":{"id":"00000000-0000-0000-0000-000000000000","title":"Herald","updatedAt":"2026-04-05T18:00:00Z","messages":[]}}}"#.data(using: .utf8)!)
        }

        defer {
            StubURLProtocol.requestHandler = nil
        }

        let tempDirectory = FileManager.default.temporaryDirectory
        let oversizedData = Data(repeating: 0x41, count: 300 * 1024)
        var attachments: [PendingAttachment] = []

        for index in 0 ..< 4 {
            let url = tempDirectory.appendingPathComponent("oversized-\(index)-\(UUID().uuidString).txt")
            try oversizedData.write(to: url)
            attachments.append(try #require(PendingAttachment.file(at: url)))
        }

        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: session
        )
        let heraldClient = LiveHeraldClient(
            apiClient: apiClient,
            accessTokenProvider: { "token" },
            allowDemoFallback: false
        )

        let response = await heraldClient.send(
            message: "Here are several attachments",
            attachments: attachments,
            clientMessageID: UUID()
        )

        #expect(requestCount.value == 0)
        #expect(response.status == .failed)
        #expect(response.content == "The attachment was too large for Herald to process. Try a smaller image.")
    }

    @Test @MainActor
    func chatStoreRetriesAttachmentOnlyMessageWithRestoredAttachments() async throws {
        final class AttachmentRetryClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?
            var lastMessage: String?
            var lastAttachments: [PendingAttachment] = []

            func connect() async {}
            func disconnect() async {}

            func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID) async -> Message {
                lastMessage = message
                lastAttachments = attachments
                return Message(sender: .herald, content: "unused", status: .delivered)
            }

            func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                lastMessage = message
                lastAttachments = attachments
                return AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: UUID()))
                        continuation.yield(.finished(Message(sender: .herald, content: "Retried", status: .delivered), nil, nil, nil))
                        continuation.finish()
                    }
                }
            }

            func loadConversation() async -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func clearConversation() async throws -> Conversation {
                Conversation(title: "Herald")
            }

            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let tempURL = FileManager.default.temporaryDirectory.appendingPathComponent("attachment-retry-\(UUID().uuidString).txt")
        let retryData = try #require("retry me".data(using: .utf8))
        try retryData.write(to: tempURL)
        let attachment = try #require(PendingAttachment.file(at: tempURL))

        let suiteName = "chat-store-attachment-retry-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let heraldClient = AttachmentRetryClient()
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        let failedMessage = Message(
            sender: .user,
            content: "[1 attachment]",
            status: .failed,
            attachments: [MessageAttachment(from: attachment)]
        )
        chatStore.conversation = Conversation(title: "Herald", messages: [failedMessage])

        await chatStore.retryMessage(failedMessage)

        #expect(heraldClient.lastMessage == "")
        #expect(heraldClient.lastAttachments.count == 1)
        #expect(heraldClient.lastAttachments.first?.fileName == attachment.fileName)
    }

    @Test @MainActor
    func chatStorePreservesUserAttachmentPreviewMetadataAfterRefresh() async throws {
        final class AttachmentRoundTripClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?

            func connect() async {}
            func disconnect() async {}

            func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "unused", status: .delivered)
            }

            func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                currentConversation = Conversation(
                    title: "Herald",
                    messages: [
                        Message(
                            id: UUID(),
                            clientMessageID: clientMessageID,
                            sender: .user,
                            content: "",
                            status: .sent,
                            attachments: attachments.map {
                                MessageAttachment(
                                    kind: $0.kind.rawValue,
                                    fileName: $0.fileName,
                                    mimeType: $0.mimeType,
                                    thumbnailBase64: $0.thumbnailBase64
                                )
                            }
                        ),
                        Message(sender: .herald, content: "I saw the attachment.", status: .delivered),
                    ]
                )

                return AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: UUID()))
                        continuation.yield(.finished(Message(sender: .herald, content: "I saw the attachment.", status: .delivered), nil, nil, nil))
                        continuation.finish()
                    }
                }
            }

            func loadConversation() async -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func clearConversation() async throws -> Conversation {
                Conversation(title: "Herald")
            }

            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
                currentConversation ?? Conversation(title: "Herald")
            }

            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let image = UIGraphicsImageRenderer(size: CGSize(width: 16, height: 16)).image { context in
            UIColor.systemBlue.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 16, height: 16))
        }
        let attachment = try #require(PendingAttachment.image(image))

        let suiteName = "chat-store-attachment-roundtrip-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let heraldClient = AttachmentRoundTripClient()
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        await chatStore.sendMessage("", attachments: [attachment])

        let userMessage = try #require(chatStore.conversation?.messages.first(where: { $0.sender == .user }))
        let mergedAttachment = try #require(userMessage.attachments.first)
        #expect(mergedAttachment.thumbnailBase64 != nil)
        #expect(mergedAttachment.localStoragePath != nil)
    }

    #if canImport(WebRTC)
    @Test @MainActor
    func liveVoiceSessionServiceRefreshesExpiredAccessTokenDuringReadiness() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: configuration)

        let accessToken = MutableBox("expired-token")
        let refreshCallCount = MutableBox(0)
        let requestCount = MutableBox(0)

        StubURLProtocol.requestHandler = { request in
            requestCount.value += 1
            let url = try #require(request.url)
            #expect(url.absoluteString == "https://relay.example.com/v1/talk/readiness")

            let authHeader = request.value(forHTTPHeaderField: "Authorization")
            if authHeader == "Bearer expired-token" {
                let response = HTTPURLResponse(url: url, statusCode: 401, httpVersion: nil, headerFields: nil)!
                let data = #"{"error":{"code":"unauthorized","message":"expired or invalid access token","retryable":false}}"#.data(using: .utf8)!
                return (response, data)
            }

            #expect(authHeader == "Bearer refreshed-token")
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
            let data = #"""
            {"data":{
              "ready":true,
              "hostOnline":true,
              "configured":true,
              "blockedReason":null,
              "preferredModels":["gpt-realtime-1.5"],
              "selectedModel":"gpt-realtime-1.5",
              "voice":"verse",
              "voiceContextUpdatedAt":"2026-04-01T20:40:47.636600Z"
            }}
            """#.data(using: .utf8)!
            return (response, data)
        }

        defer {
            StubURLProtocol.requestHandler = nil
        }

        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: session
        )
        let voiceService = LiveVoiceSessionService(
            apiClient: apiClient,
            accessTokenProvider: { accessToken.value },
            accessTokenRefresher: {
                refreshCallCount.value += 1
                accessToken.value = "refreshed-token"
                return accessToken.value
            },
            urlSession: session
        )

        await voiceService.refreshReadiness()

        #expect(refreshCallCount.value == 1)
        #expect(requestCount.value == 2)
        #expect(voiceService.canStartSession)
        #expect(voiceService.connectionState == .ready)
        #expect(voiceService.statusMessage == "Herald talk is ready.")
        #expect(voiceService.blockedReason == nil)
    }

    @Test @MainActor
    func liveVoiceSessionServiceInterruptsAssistantPlaybackOnSpeechStart() async throws {
        let sentEvents = MutableBox([[String: Any]]())
        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" }
        )
        let voiceService = LiveVoiceSessionService(
            apiClient: apiClient,
            accessTokenProvider: { "token" },
            realtimeEventTransportOverride: { data in
                guard let payload = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                    return false
                }
                sentEvents.value.append(payload)
                return true
            }
        )

        voiceService.connectionState = .connected
        voiceService.handleDataChannelEvent(
            [
                "type": "response.created",
                "response": ["id": "resp_123"],
            ]
        )
        voiceService.handleDataChannelEvent(
            [
                "type": "conversation.item.created",
                "item": [
                    "id": "item_123",
                    "role": "assistant",
                    "type": "message",
                ],
            ]
        )
        voiceService.handleDataChannelEvent(
            [
                "type": "response.output_text.delta",
                "delta": "Testing interruption handling.",
            ]
        )
        voiceService.handleDataChannelEvent(["type": "output_audio_buffer.started"])
        try? await Task.sleep(for: .milliseconds(25))

        voiceService.handleDataChannelEvent(["type": "input_audio_buffer.speech_started"])

        #expect(sentEvents.value.count == 3)
        #expect(sentEvents.value[0]["type"] as? String == "response.cancel")
        #expect(sentEvents.value[0]["response_id"] as? String == "resp_123")
        #expect(sentEvents.value[1]["type"] as? String == "output_audio_buffer.clear")
        #expect(sentEvents.value[2]["type"] as? String == "conversation.item.truncate")
        #expect(sentEvents.value[2]["item_id"] as? String == "item_123")
        #expect(sentEvents.value[2]["content_index"] as? Int == 0)
        let audioEndMs = try #require(sentEvents.value[2]["audio_end_ms"] as? Int)
        #expect(audioEndMs >= 0)
        #expect(voiceService.voiceState == .listening)
        #expect(voiceService.statusMessage == "Listening")
        #expect(voiceService.transcriptItems.last?.isPartial == false)
    }

    @Test @MainActor
    func liveVoiceSessionServiceDoesNotInterruptWhenAssistantIsNotSpeaking() async throws {
        let sentEvents = MutableBox([[String: Any]]())
        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" }
        )
        let voiceService = LiveVoiceSessionService(
            apiClient: apiClient,
            accessTokenProvider: { "token" },
            realtimeEventTransportOverride: { data in
                guard let payload = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                    return false
                }
                sentEvents.value.append(payload)
                return true
            }
        )

        voiceService.connectionState = .connected
        voiceService.handleDataChannelEvent(
            [
                "type": "response.created",
                "response": ["id": "resp_456"],
            ]
        )
        voiceService.handleDataChannelEvent(
            [
                "type": "conversation.item.created",
                "item": [
                    "id": "item_456",
                    "role": "assistant",
                    "type": "message",
                ],
            ]
        )

        voiceService.handleDataChannelEvent(["type": "input_audio_buffer.speech_started"])

        #expect(sentEvents.value.isEmpty)
        #expect(voiceService.voiceState == .listening)
    }

    @Test @MainActor
    func liveVoiceSessionServiceRecoversFromInterruptionsWithoutEndingSession() async throws {
        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" }
        )
        let voiceService = LiveVoiceSessionService(
            apiClient: apiClient,
            accessTokenProvider: { "token" }
        )

        voiceService.connectionState = .connected
        voiceService.voiceState = .speaking

        voiceService.handleAudioInterruptionBegan()

        #expect(voiceService.voiceState == .interrupted)
        #expect(voiceService.statusMessage == "Audio interrupted.")

        voiceService.handleAudioInterruptionEnded(shouldResume: true)

        #expect(voiceService.connectionState == .connected)
        #expect(voiceService.voiceState == .listening)
        #expect(voiceService.statusMessage == "Listening")
    }

    @Test @MainActor
    func liveVoiceSessionServiceRecoversFromRouteChangesDuringActiveSession() async throws {
        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" }
        )
        let voiceService = LiveVoiceSessionService(
            apiClient: apiClient,
            accessTokenProvider: { "token" }
        )

        voiceService.connectionState = .connected
        voiceService.voiceState = .interrupted

        voiceService.handleAudioRouteChange(.oldDeviceUnavailable)

        #expect(voiceService.connectionState == .connected)
        #expect(voiceService.voiceState == .listening)
        #expect(voiceService.statusMessage == "Audio route changed.")
    }

    @Test @MainActor
    func liveVoiceSessionServiceKeepsUserTranscriptOrderedWhenTranscriptionFinishesLate() async throws {
        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" }
        )
        let voiceService = LiveVoiceSessionService(
            apiClient: apiClient,
            accessTokenProvider: { "token" }
        )

        voiceService.connectionState = .connected
        voiceService.handleDataChannelEvent([
            "type": "input_audio_buffer.committed",
            "item_id": "user_item_1",
        ])
        voiceService.handleDataChannelEvent([
            "type": "response.created",
            "response": ["id": "resp_late_user"],
        ])
        voiceService.handleDataChannelEvent([
            "type": "conversation.item.created",
            "item": [
                "id": "assistant_item_1",
                "role": "assistant",
                "type": "message",
            ],
        ])
        voiceService.handleDataChannelEvent([
            "type": "response.output_text.delta",
            "delta": "Let me check that.",
        ])

        voiceService.handleDataChannelEvent([
            "type": "conversation.item.input_audio_transcription.completed",
            "item_id": "user_item_1",
            "transcript": "What should I focus on today?",
        ])

        #expect(voiceService.transcriptItems.count == 2)
        #expect(voiceService.transcriptItems[0].speaker == .user)
        #expect(voiceService.transcriptItems[0].text == "What should I focus on today?")
        #expect(voiceService.transcriptItems[0].isPartial == false)
        #expect(voiceService.transcriptItems[1].speaker == .herald)
        #expect(voiceService.transcriptItems[1].text == "Let me check that.")
    }

    @Test @MainActor
    func liveVoiceSessionServiceIgnoresLateRealtimeErrorsAfterIntentionalEnd() async throws {
        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" }
        )
        let voiceService = LiveVoiceSessionService(
            apiClient: apiClient,
            accessTokenProvider: { "token" }
        )

        voiceService.connectionState = .connected
        voiceService.voiceState = .speaking

        await voiceService.endSession()
        voiceService.handleDataChannelEvent([
            "type": "error",
            "error": ["message": "Connection lost."],
        ])

        #expect(voiceService.connectionState == .idle)
        #expect(voiceService.voiceState == .idle)
        #expect(voiceService.statusMessage == nil)
    }
    #endif

    @Test @MainActor
    func liveHeraldClientRefreshesExpiredAccessTokenDuringConversationLoad() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: configuration)

        let accessToken = MutableBox("expired-token")
        let refreshCallCount = MutableBox(0)
        let requestCount = MutableBox(0)
        let conversationID = UUID()

        StubURLProtocol.requestHandler = { request in
            requestCount.value += 1
            let url = try #require(request.url)
            #expect(url.absoluteString == "https://relay.example.com/v1/conversations/current")

            let authHeader = request.value(forHTTPHeaderField: "Authorization")
            if authHeader == "Bearer expired-token" {
                let response = HTTPURLResponse(url: url, statusCode: 401, httpVersion: nil, headerFields: nil)!
                let data = #"{"error":{"code":"unauthorized","message":"expired or invalid access token","retryable":false}}"#.data(using: .utf8)!
                return (response, data)
            }

            #expect(authHeader == "Bearer refreshed-token")
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
            let data = #"""
            {"data":{
              "conversation":{
                "id":"\#(conversationID.uuidString)",
                "title":"Herald",
                "updatedAt":"2026-04-03T21:15:00Z",
                "messages":[]
              }
            }}
            """#.data(using: .utf8)!
            return (response, data)
        }

        defer {
            StubURLProtocol.requestHandler = nil
        }

        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: session
        )
        let heraldClient = LiveHeraldClient(
            apiClient: apiClient,
            accessTokenProvider: { accessToken.value },
            accessTokenRefresher: {
                refreshCallCount.value += 1
                accessToken.value = "refreshed-token"
                return accessToken.value
            },
            allowDemoFallback: false
        )

        let conversation = await heraldClient.loadConversation()

        #expect(refreshCallCount.value == 1)
        #expect(requestCount.value == 2)
        #expect(conversation.id == conversationID)
        #expect(heraldClient.connectionStatus == .connected)
    }

    @Test @MainActor
    func liveHeraldHostServiceRefreshesExpiredAccessTokenDuringFetch() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: configuration)

        let accessToken = MutableBox("expired-token")
        let refreshCallCount = MutableBox(0)
        let requestCount = MutableBox(0)
        let hostID = UUID()

        StubURLProtocol.requestHandler = { request in
            requestCount.value += 1
            let url = try #require(request.url)
            #expect(url.absoluteString == "https://relay.example.com/v1/hosts/current")

            let authHeader = request.value(forHTTPHeaderField: "Authorization")
            if authHeader == "Bearer expired-token" {
                let response = HTTPURLResponse(url: url, statusCode: 401, httpVersion: nil, headerFields: nil)!
                let data = #"{"error":{"code":"unauthorized","message":"expired or invalid access token","retryable":false}}"#.data(using: .utf8)!
                return (response, data)
            }

            #expect(authHeader == "Bearer refreshed-token")
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
            let data = #"""
            {"data":{
              "host":{
                "id":"\#(hostID.uuidString)",
                "displayName":"Home Mac mini",
                "hostname":"test-host",
                "platform":"macos",
                "connectorVersion":"0.1.0",
                "heraldCommand":"/usr/local/bin/hermes",
                "heraldVersion":"hermes 0.7.0",
                "lastSeenAt":"2026-04-03T21:15:00Z",
                "lastConnectedAt":"2026-04-03T21:10:00Z",
                "isOnline":true
              }
            }}
            """#.data(using: .utf8)!
            return (response, data)
        }

        defer {
            StubURLProtocol.requestHandler = nil
        }

        let apiClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: session
        )
        let hostService = LiveHeraldHostService(
            apiClient: apiClient,
            accessTokenRefresher: {
                refreshCallCount.value += 1
                accessToken.value = "refreshed-token"
                return accessToken.value
            }
        )

        let host = try await hostService.fetchCurrentHost(accessToken: accessToken.value)

        #expect(refreshCallCount.value == 1)
        #expect(requestCount.value == 2)
        #expect(host?.id == hostID)
        #expect(host?.isOnline == true)
    }

    @Test @MainActor
    func settingsStoreSanitizesDisallowedReleaseEnvironmentToProduction() async throws {
        let suiteName = "settings-store-release-policy-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        persistence.saveUserSettings(
            UserSettings(
                userName: "Alex",
                avatarInitials: "A",
                notificationsEnabled: true,
                hapticFeedbackEnabled: true,
                environment: .staging,
                autoConnectOnLaunch: true
            )
        )

        let settingsStore = SettingsStore(
            persistence: persistence,
            environmentPolicy: AppEnvironmentPolicy(allowsEnvironmentOverrides: false)
        )

        #expect(settingsStore.settings.environment == .production)
        #expect(settingsStore.availableEnvironments == [.production])
    }

    @Test @MainActor
    func settingsStorePersistsCustomRelayConfiguration() async throws {
        let suiteName = "settings-store-relay-config-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let settingsStore = SettingsStore(
            persistence: persistence,
            buildConfiguration: AppBuildConfiguration(infoDictionary: [:])
        )

        settingsStore.settings.relayConfiguration = RelayConfiguration(
            relayMode: .custom,
            customRelayBaseURL: "https://demo.example.com/v1",
            hostedRelayBaseURL: nil,
            hostedRelayEnabled: false
        )

        let reloaded = persistence.loadUserSettings()
        #expect(reloaded?.relayConfiguration.activeBaseURLString == "https://demo.example.com/v1")
    }

    @Test
    func relayConfigurationDefaultsCustomRelayToSelfHostedConnectionMode() throws {
        let configuration = RelayConfiguration(
            relayMode: .custom,
            customRelayBaseURL: "https://relay.example.com/v1",
            hostedRelayBaseURL: nil,
            hostedRelayEnabled: false
        )

        #expect(configuration.connectionMode == .selfHostedRelay)
        #expect(configuration.connectionMode.reliesOnOfficialPushRelay == false)
        #expect(configuration.activeBaseURLString == "https://relay.example.com/v1")
    }

    @Test
    func relayConfigurationDefaultsToSelfHostedRelay() throws {
        let buildConfiguration = AppBuildConfiguration(infoDictionary: [
            "APP_HOSTED_RELAY_URL": "https://your-relay.example.com/v1",
            "APP_HOSTED_RELAY_ENABLED": true,
        ])

        let configuration = RelayConfiguration.defaultValue(
            buildConfiguration: buildConfiguration,
            environmentPolicy: AppEnvironmentPolicy(allowsEnvironmentOverrides: true)
        )

        #expect(configuration.connectionMode == .selfHostedRelay)
    }

    @Test
    func productionSettingsWithLocalhostRemainSelfHostedAfterPolicyApplication() throws {
        let buildConfiguration = AppBuildConfiguration(infoDictionary: [
            "APP_HOSTED_RELAY_URL": "https://your-relay.example.com/v1",
            "APP_HOSTED_RELAY_ENABLED": true,
        ])
        let settings = UserSettings(
            environment: .production,
            relayConfiguration: RelayConfiguration(
                connectionMode: .selfHostedRelay,
                customRelayBaseURL: AppEnvironment.development.baseURLString
            )
        )

        let migrated = settings.applyingEnvironmentPolicy(
            AppEnvironmentPolicy(allowsEnvironmentOverrides: true),
            buildConfiguration: buildConfiguration
        )

        #expect(migrated.relayConfiguration.connectionMode == .selfHostedRelay)
    }

    @Test
    func relayConfigurationMigratesLegacyHostedModeToSelfHostedConnectionMode() throws {
        let json = """
        {
          "relayMode": "hosted",
          "customRelayBaseURL": "https://managed.example.com/v1",
          "hostedRelayBaseURL": "https://managed.example.com/v1",
          "hostedRelayEnabled": true
        }
        """
        let data = try #require(json.data(using: .utf8))

        let configuration = try JSONDecoder().decode(RelayConfiguration.self, from: data)

        #expect(configuration.connectionMode == .selfHostedRelay)
        #expect(configuration.connectionMode.reliesOnOfficialPushRelay == false)
        #expect(configuration.activeBaseURLString == "https://managed.example.com/v1")
    }

    @Test
    func relayConfigurationTailscaleModeUsesCustomRelayURLWithoutOfficialPushByDefault() throws {
        let configuration = RelayConfiguration(
            connectionMode: .tailscale,
            customRelayBaseURL: "https://home-mac.tailnet.ts.net/v1",
            hostedRelayBaseURL: "https://managed.example.com/v1",
            hostedRelayEnabled: true
        )

        #expect(configuration.relayMode == .custom)
        #expect(configuration.connectionMode == .tailscale)
        #expect(configuration.connectionMode.reliesOnOfficialPushRelay == false)
        #expect(configuration.activeBaseURLString == "https://home-mac.tailnet.ts.net/v1")
    }

    @Test
    func appBuildConfigurationParsesManagedPushBrokerKeys() throws {
        let configuration = AppBuildConfiguration(infoDictionary: [
            "APP_HOSTED_RELAY_URL": FakeBundleInfo.hostedRelayURL,
            "APP_HOSTED_RELAY_ENABLED": true,
            "APP_PUSH_TRANSPORT": "relay",
            "APP_PUSH_BROKER_URL": FakeBundleInfo.pushBrokerURL,
        ])

        #expect(configuration.hostedRelayBaseURL == FakeBundleInfo.hostedRelayURL)
        #expect(configuration.hostedRelayEnabled == true)
        #expect(configuration.pushTransport == .relay)
        #expect(configuration.pushBrokerBaseURL?.absoluteString == FakeBundleInfo.pushBrokerURL)
        #expect(configuration.usesManagedPushBroker == true)
    }

    @Test @MainActor
    func pushRegistrationCoordinatorUsesDirectTransport() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: configuration)

        let requestLog = MutableBox<[String]>([])
        let relayRegisterBodies = MutableBox<[String]>([])

        StubURLProtocol.requestHandler = { request in
            let url = try #require(request.url)
            requestLog.value.append(url.absoluteString)
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!

            if url.absoluteString == "https://relay.example.com/v1/push/register" {
                let body = requestBodyString(request)
                relayRegisterBodies.value.append(body)
                #expect(body.contains("\"transport\":\"direct\""))
                #expect(body.contains("\"apnsToken\":\"abcd1234efef5678\""))
                let data = #"{"data":{"registered":true}}"#.data(using: .utf8)!
                return (response, data)
            }

            throw URLError(.badURL)
        }

        defer { StubURLProtocol.requestHandler = nil }

        let relayAPIClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: session
        )
        let secureStore = MockSecureStore()
        let registrationStore = PushBrokerRegistrationStore(secureStore: secureStore)
        let coordinator = PushRegistrationCoordinator(
            relayAPIClient: relayAPIClient,
            brokerClient: nil,
            registrationStore: registrationStore,
            appAttestService: FakeAppAttestService(),
            buildConfiguration: AppBuildConfiguration(infoDictionary: [:])
        )

        let didRegister = try await coordinator.registerPushToken(
            "abcd1234efef5678",
            relayConfiguration: RelayConfiguration(connectionMode: .selfHostedRelay, customRelayBaseURL: "https://relay.example.com/v1"),
            accessToken: "access-token",
            deviceID: UUID(uuidString: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")!,
            installationID: UUID(uuidString: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")!,
            bundleID: "net.fihonline.herald",
            appVersion: "1.1.0",
            pushEnvironment: "production"
        )

        #expect(didRegister)
        #expect(requestLog.value == ["https://relay.example.com/v1/push/register"])
        #expect(relayRegisterBodies.value.count == 1)
    }

    @Test @MainActor
    func pushRegistrationCoordinatorAlwaysUsesDirectTransportRegardlessOfCachedState() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: configuration)

        let requestLog = MutableBox<[String]>([])

        StubURLProtocol.requestHandler = { request in
            let url = try #require(request.url)
            requestLog.value.append(url.absoluteString)
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!

            if url.absoluteString == "https://relay.example.com/v1/push/register" {
                let data = #"{"data":{"registered":true}}"#.data(using: .utf8)!
                return (response, data)
            }

            throw URLError(.badURL)
        }

        defer { StubURLProtocol.requestHandler = nil }

        let relayAPIClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: session
        )
        let secureStore = MockSecureStore()
        let registrationStore = PushBrokerRegistrationStore(secureStore: secureStore)
        // Pre-populate with stale broker state — should be ignored since direct transport is always used.
        await registrationStore.saveRegistrationState(
            PushBrokerRegistrationState(
                relayHandle: "relay-handle-123",
                sendGrant: "relay-send-grant-123",
                relayID: "relay-123",
                relayPublicKey: "relay-pub-key",
                brokerBaseURL: "https://broker.example.com/v1",
                installationID: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                tokenHash: PushBrokerRegistrationState.tokenHash(for: "abcd1234efef5678"),
                tokenDebugSuffix: "efef5678",
                expiresAt: Date.distantFuture
            )
        )
        let coordinator = PushRegistrationCoordinator(
            relayAPIClient: relayAPIClient,
            brokerClient: nil,
            registrationStore: registrationStore,
            appAttestService: FakeAppAttestService(),
            buildConfiguration: AppBuildConfiguration(infoDictionary: [:])
        )

        let didRegister = try await coordinator.registerPushToken(
            "abcd1234efef5678",
            relayConfiguration: RelayConfiguration(connectionMode: .selfHostedRelay, customRelayBaseURL: "https://relay.example.com/v1"),
            accessToken: "access-token",
            deviceID: UUID(uuidString: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")!,
            installationID: UUID(uuidString: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")!,
            bundleID: "net.fihonline.herald",
            appVersion: "1.1.0",
            pushEnvironment: "production"
        )

        #expect(didRegister)
        #expect(requestLog.value == ["https://relay.example.com/v1/push/register"])
    }

    @Test @MainActor
    func pushRegistrationCoordinatorDeactivatesRelayRegistrationAndClearsCachedBrokerState() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: configuration)

        let requestLog = MutableBox<[String]>([])
        StubURLProtocol.requestHandler = { request in
            let url = try #require(request.url)
            requestLog.value.append(url.absoluteString)
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
            let data = #"{"data":{"deactivated":true}}"#.data(using: .utf8)!
            return (response, data)
        }
        defer { StubURLProtocol.requestHandler = nil }

        let relayAPIClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: session
        )
        let registrationStore = PushBrokerRegistrationStore(secureStore: MockSecureStore())
        await registrationStore.saveRegistrationState(
            PushBrokerRegistrationState(
                relayHandle: "relay-handle-123",
                sendGrant: "relay-send-grant-123",
                relayID: "relay-123",
                relayPublicKey: "relay-pub-key",
                brokerBaseURL: FakeBundleInfo.pushBrokerURL,
                installationID: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                tokenHash: PushBrokerRegistrationState.tokenHash(for: "abcd1234efef5678"),
                tokenDebugSuffix: "efef5678",
                expiresAt: Date.distantFuture
            )
        )
        let coordinator = PushRegistrationCoordinator(
            relayAPIClient: relayAPIClient,
            brokerClient: nil,
            registrationStore: registrationStore,
            appAttestService: FakeAppAttestService(),
            buildConfiguration: AppBuildConfiguration(infoDictionary: [:])
        )

        try await coordinator.deactivatePushRegistration(accessToken: "access-token")

        #expect(requestLog.value == ["https://relay.example.com/v1/push/deactivate"])
        #expect(await registrationStore.loadRegistrationState() == nil)
    }

    @Test
    func relayConnectionModesExposeModeAwareChatRecoveryCopy() throws {
        #expect(RelayConnectionMode.tailscale.defaultOfflineMessage == "Open Tailscale or reconnect to your tailnet to reach Herald.")
        #expect(RelayConnectionMode.selfHostedRelay.notConnectedMessage == "Pair a Hermes host with this self-hosted relay before sending messages.")
    }

    @Test
    func relayConnectionModesExposeModeAwareUnreachableSendGuidance() throws {
        #expect(
            RelayConnectionMode.tailscale.unreachableSendBlockedMessage ==
            "Can't reach your tailnet relay. Open Tailscale to reconnect, then send again."
        )
        #expect(
            RelayConnectionMode.selfHostedRelay.unreachableSendBlockedMessage ==
            "Your self-hosted relay URL is not reachable. Check the URL in Settings and try again."
        )
    }

    @Test
    func relayConnectionModesExposeUnreachableActionLabels() throws {
        #expect(RelayConnectionMode.tailscale.unreachableActionLabel == "Open Tailscale")
        #expect(RelayConnectionMode.selfHostedRelay.unreachableActionLabel == "Retry")
    }

    @Test
    func tailscaleModeProvidesDeepLinkSelfHostedFallsBackToRetry() throws {
        #expect(RelayConnectionMode.selfHostedRelay.unreachableActionDeepLink == nil)
        let tailscaleDeepLink = RelayConnectionMode.tailscale.unreachableActionDeepLink
        #expect(tailscaleDeepLink == URL(string: "tailscale://"))
        #expect(tailscaleDeepLink?.scheme == "tailscale")
    }

    @Test
    func relayURLHintShownForAllModes() throws {
        let tailscaleHint = try #require(RelayConnectionMode.tailscale.relayURLHint)
        #expect(tailscaleHint.contains("tail-scale.ts.net"))
        #expect(tailscaleHint.contains("tailscale serve"))
        let selfHostedHint = try #require(RelayConnectionMode.selfHostedRelay.relayURLHint)
        #expect(selfHostedHint.contains("public Hermes relay"))
    }

    @Test
    func backgroundDeliveryNotesStayHonestAboutPushCapability() throws {
        let tailscaleNote = RelayConnectionMode.tailscale.backgroundDeliveryNote
        #expect(tailscaleNote.contains("No official background push"))
        #expect(tailscaleNote.contains("foreground"))
        let selfHostedNote = RelayConnectionMode.selfHostedRelay.backgroundDeliveryNote
        #expect(selfHostedNote.contains("don't receive official push credentials"))
    }

    @Test
    func relayConnectionModePreservesLegacyMappingForBackwardsCompatibility() throws {
        #expect(RelayConnectionMode(legacyRelayMode: .hosted) == .selfHostedRelay)
        #expect(RelayConnectionMode(legacyRelayMode: .custom) == .selfHostedRelay)
        #expect(RelayConnectionMode.tailscale.legacyRelayMode == .custom)
        #expect(RelayConnectionMode.selfHostedRelay.legacyRelayMode == .custom)
    }

    @Test
    func noModeReliesOnOfficialPushRelay() throws {
        #expect(RelayConnectionMode.tailscale.reliesOnOfficialPushRelay == false)
        #expect(RelayConnectionMode.selfHostedRelay.reliesOnOfficialPushRelay == false)
    }

    @Test
    func selectableConnectionModesAlwaysTailscaleAndSelfHosted() throws {
        let config = RelayConfiguration(
            connectionMode: .selfHostedRelay,
            customRelayBaseURL: "https://relay.example.com/v1",
            hostedRelayBaseURL: nil,
            hostedRelayEnabled: false
        )
        #expect(config.selectableConnectionModes == [.tailscale, .selfHostedRelay])
    }

    @Test
    func relayConfigurationDefaultsToSelfHostedWhenNoHostedURL() throws {
        let config = RelayConfiguration(
            connectionMode: .selfHostedRelay,
            customRelayBaseURL: "https://relay.example.com/v1",
            hostedRelayBaseURL: nil,
            hostedRelayEnabled: false
        )
        #expect(config.connectionMode == .selfHostedRelay)
        #expect(config.relayMode == .custom)
    }

    @Test
    func legacyManagedRelayDecodesToSelfHosted() throws {
        let json = #""managedRelay""#
        let data = try #require(json.data(using: .utf8))
        let mode = try JSONDecoder().decode(RelayConnectionMode.self, from: data)
        #expect(mode == .selfHostedRelay)
    }

    @Test
    func freshInstallDefaultsToSelfHostedRelay() throws {
        let settings = UserSettings()
        #expect(settings.relayConfiguration.connectionMode == .selfHostedRelay)
    }

    @Test
    func relayDecoderParsesFractionalSecondsWithoutTimezone() throws {
        let data = #"{"timestamp":"2026-03-31T18:58:36.197800"}"#.data(using: .utf8)!
        let payload = try RelayCoders.makeDecoder().decode(TimestampPayload.self, from: data)
        let expected = Date(timeIntervalSince1970: 1774983516.1978)

        #expect(abs(payload.timestamp.timeIntervalSince(expected)) < 0.000_001)
    }

    @Test
    func relayDecoderParsesTimezoneQualifiedDates() throws {
        let data = #"{"timestamp":"2026-03-31T18:58:36Z"}"#.data(using: .utf8)!
        let payload = try RelayCoders.makeDecoder().decode(TimestampPayload.self, from: data)

        #expect(payload.timestamp == Date(timeIntervalSince1970: 1774983516))
    }

    @Test @MainActor
    func persistenceStorePersistsAndClearsHealthQueryAnchors() async throws {
        let suiteName = "health-anchors-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let anchorData = Data([0x01, 0x02, 0x03])

        persistence.saveHealthQueryAnchorData(anchorData, for: "steps")
        persistence.saveHealthQueryAnchorData(Data([0x04]), for: "heart_rate")

        #expect(persistence.loadHealthQueryAnchorData(for: "steps") == anchorData)
        #expect(persistence.loadHealthQueryAnchorData(for: "heart_rate") == Data([0x04]))

        persistence.clearHealthQueryAnchorData()

        #expect(persistence.loadHealthQueryAnchorData(for: "steps") == nil)
        #expect(persistence.loadHealthQueryAnchorData(for: "heart_rate") == nil)
    }

    @Test
    func phonePairingCodeNormalizesAndFormatsManualEntry() throws {
        let normalized = try PhonePairingCode.normalize("ab cd-efgh")

        #expect(normalized == "ABCDEFGH")
        #expect(PhonePairingCode.format("ab cd-efgh") == "ABCD-EFGH")
        #expect(PhonePairingCode.isComplete("ABCD-EFGH"))
    }

    @Test @MainActor
    func pairingStorePersistsRelayConfigurationAndTokens() async throws {
        let suiteName = "pairing-store-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        let sessionStore = AppSessionStore(
            bootstrapService: MockSessionBootstrapService(),
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .production }
        )
        let pairingStore = PairingStore(
            pairingService: RecordingPairingService(),
            sessionStore: sessionStore,
            persistence: persistence,
            environmentProvider: { .production },
            relayBaseURLProvider: { "https://relay.example.test/v1" }
        )

        let setupCode = makeSetupCode()
        let didPair = await pairingStore.pair(using: setupCode)

        #expect(didPair)
        #expect(pairingStore.pairedRelayConfiguration?.hostDisplayName == "relay.example.test")
        #expect(persistence.loadPairedRelayConfiguration()?.baseURLString == "https://relay.example.test/v1")
        #expect(await secureStore.retrieve(key: "session.accessToken") == "paired-access-token-ABCDEFGH")
        #expect(sessionStore.state.displayName == "Morgan")
    }

    @Test @MainActor
    func hostStoreGeneratesEnrollmentCodeAndClearsOnRevoke() async throws {
        let service = RecordingHeraldHostService()
        service.currentHost = HeraldHostStatus(
            id: UUID(),
            displayName: "Home Mac mini",
            hostname: "test-host",
            platform: "macos",
            connectorVersion: "0.1.0",
            heraldCommand: "herald",
            heraldVersion: "hermes 1.2.3",
            heraldModel: "gpt-5.4-mini",
            lastSeenAt: .now,
            lastConnectedAt: .now,
            isOnline: false
        )

        let hostStore = HeraldHostStore(
            hostService: service,
            accessTokenProvider: { "access-token" }
        )

        await hostStore.refresh()
        #expect(hostStore.currentHost?.resolvedDisplayName == "Home Mac mini")

        await hostStore.generateEnrollmentCode()
        #expect(hostStore.activeEnrollmentCode?.setupCode == "HC1:test-setup-code")

        await hostStore.revokeCurrentHost()
        #expect(hostStore.currentHost == nil)
        #expect(hostStore.activeEnrollmentCode == nil)
    }

    @Test @MainActor
    func hostStoreMarksReachabilityErrorsWithoutPretendingHostIsOffline() async throws {
        let service = RecordingHeraldHostService()
        service.fetchError = RelayAPIClient.ClientError.requestFailed("Relay unreachable.")

        let hostStore = HeraldHostStore(
            hostService: service,
            accessTokenProvider: { "access-token" }
        )

        await hostStore.refresh()

        #expect(hostStore.currentHost == nil)
        #expect(hostStore.connectionState == .unreachable)
        #expect(hostStore.lastErrorMessage == "Relay unreachable.")
    }

    @Test @MainActor
    func hostStoreKeepsKnownOnlineHostDuringRefreshErrors() async throws {
        let service = RecordingHeraldHostService()
        service.currentHost = HeraldHostStatus(
            id: UUID(),
            displayName: "Home Mac mini",
            hostname: "test-host",
            platform: "macos",
            connectorVersion: "0.1.0",
            heraldCommand: "herald",
            heraldVersion: "hermes 1.2.3",
            heraldModel: "gpt-5.4-mini",
            lastSeenAt: .now,
            lastConnectedAt: .now,
            isOnline: true
        )

        let hostStore = HeraldHostStore(
            hostService: service,
            accessTokenProvider: { "access-token" }
        )

        await hostStore.refresh()
        service.fetchError = RelayAPIClient.ClientError.requestFailed("Relay unreachable.")
        await hostStore.refresh()

        #expect(hostStore.currentHost?.resolvedDisplayName == "Home Mac mini")
        #expect(hostStore.connectionState == .online)
        #expect(hostStore.lastErrorMessage == "Relay unreachable.")
    }

    @Test @MainActor
    func pairingStoreDisconnectClearsRelayConfigurationAndSession() async throws {
        let suiteName = "pairing-store-disconnect-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        let sessionStore = AppSessionStore(
            bootstrapService: MockSessionBootstrapService(),
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .production }
        )
        let pairingStore = PairingStore(
            pairingService: RecordingPairingService(),
            sessionStore: sessionStore,
            persistence: persistence,
            environmentProvider: { .production },
            relayBaseURLProvider: { "https://relay.example.test/v1" }
        )

        let setupCode = makeSetupCode()
        _ = await pairingStore.pair(using: setupCode)

        await pairingStore.disconnect()

        #expect(pairingStore.pairedRelayConfiguration == nil)
        #expect(persistence.loadPairedRelayConfiguration() == nil)
        #expect(await secureStore.retrieve(key: "session.accessToken") == nil)
        #expect(sessionStore.state.deviceRegistered == false)
    }

    @Test @MainActor
    func inboxStorePersistsReadAndDismissState() async throws {
        let suiteName = "inbox-store-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let sessionStore = AppSessionStore(
            bootstrapService: MockSessionBootstrapService(),
            syncCoordinator: MockSyncCoordinator(),
            secureStore: MockSecureStore(),
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )
        await sessionStore.bootstrap()

        let inboxStore = InboxStore(
            inboxService: MockInboxService(),
            persistence: persistence,
            sessionStore: sessionStore
        )

        await inboxStore.loadInbox(force: true)
        let originalItems = inboxStore.items

        guard let firstItem = originalItems.first, let secondItem = originalItems.dropFirst().first else {
            Issue.record("Expected demo inbox items")
            return
        }

        await inboxStore.performPrimaryAction(for: firstItem)
        await inboxStore.dismiss(secondItem)

        let reloadedStore = InboxStore(
            inboxService: MockInboxService(),
            persistence: persistence,
            sessionStore: sessionStore
        )

        await reloadedStore.loadInbox(force: true)

        #expect(reloadedStore.items.contains(where: { $0.stableIdentifier == firstItem.stableIdentifier && $0.isRead }))
        #expect(!reloadedStore.items.contains(where: { $0.stableIdentifier == secondItem.stableIdentifier }))
    }

    @Test
    func sensorOutboxStateDeduplicatesLocationAndWindowedHealthSnapshots() {
        var outbox = SensorOutboxState()
        let now = Date(timeIntervalSince1970: 1_774_983_516)

        outbox.enqueue(
            location: LocationUpdate(
                latitude: 40.0,
                longitude: -73.0,
                altitude: nil,
                accuracy: 20,
                timestamp: now
            )
        )
        outbox.enqueue(
            location: LocationUpdate(
                latitude: 41.0,
                longitude: -74.0,
                altitude: nil,
                accuracy: 15,
                timestamp: now.addingTimeInterval(30)
            )
        )

        outbox.enqueue(
            healthSamples: [
                HealthSnapshot.Sample(
                    metric: "steps",
                    value: 1000,
                    unit: "count",
                    startAt: now,
                    endAt: now.addingTimeInterval(300)
                ),
                HealthSnapshot.Sample(
                    metric: "steps",
                    value: 1200,
                    unit: "count",
                    startAt: now,
                    endAt: now.addingTimeInterval(600)
                ),
                HealthSnapshot.Sample(
                    metric: "heart_rate",
                    value: 72,
                    unit: "bpm",
                    startAt: now,
                    endAt: nil
                )
            ]
        )

        #expect(outbox.pendingLocation?.latitude == 41.0)
        #expect(outbox.pendingHealthSamples.count == 2)
        #expect(outbox.pendingHealthSamples.first(where: { $0.metric == "steps" })?.value == 1200)
    }

    @Test @MainActor
    func persistenceStoreRoundTripsSensorOutboxState() {
        let suiteName = "sensor-outbox-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let date = Date(timeIntervalSince1970: 1_774_983_516)
        let outbox = SensorOutboxState(
            pendingLocation: .init(
                latitude: 40.0,
                longitude: -73.0,
                altitude: 12,
                accuracy: 20,
                recordedAt: date
            ),
            pendingHealthSamples: [
                .init(
                    metric: "heart_rate",
                    value: 72,
                    unit: "bpm",
                    startAt: date,
                    endAt: nil
                )
            ]
        )

        persistence.saveSensorOutboxState(outbox)

        let reloaded = persistence.loadSensorOutboxState()
        #expect(reloaded == outbox)
    }

    @Test @MainActor
    func chatStoreLoadsLatestUsageFromConversationMetadata() async {
        let heraldClient = RecordingHeraldClient()
        heraldClient.currentConversation = Conversation(
            title: "Herald",
            messages: [
                Message(sender: .user, content: "Hello"),
                Message(sender: .herald, content: "Hi")
            ],
            latestUsage: TokenUsage(
                promptTokens: 3200,
                completionTokens: 240,
                totalTokens: 3440
            )
        )

        let suiteName = "chat-usage-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)

        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)
        await chatStore.loadConversation()

        #expect(chatStore.lastTokenUsage?.promptTokens == 3200)
        #expect(chatStore.currentContextTokens == 3200)
    }

    // FIXME(B36): inferredContextWindow was removed from ChatStore; test needs rewriting.
    /*
    @Test @MainActor
    func chatStoreInfersHeraldAlignedContextWindowFallback() {
        #expect(ChatStore.inferredContextWindow(for: "gpt-5.4-mini") == 128_000)
        #expect(ChatStore.inferredContextWindow(for: "claude-sonnet-4.6") == 1_000_000)
    }
    */

    // MARK: - Expired Token Recovery Tests

    @Test @MainActor
    func sessionBootstrapRecoversWithValidAccessToken() async throws {
        let suiteName = "session-bootstrap-valid-token-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        await secureStore.store(key: "session.accessToken", value: "valid-token")
        await secureStore.store(key: "session.refreshToken", value: "valid-refresh")

        let bootstrapService = RecordingSessionBootstrapService()
        let sessionStore = AppSessionStore(
            bootstrapService: bootstrapService,
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )

        await sessionStore.bootstrap()

        #expect(sessionStore.state.connectionStatus == .connected)
        #expect(sessionStore.launchState == .ready)
        #expect(bootstrapService.registerCallCount == 0)
    }

    @Test @MainActor
    func sessionBootstrapRecoversExpiredAccessTokenViaRefresh() async throws {
        let suiteName = "session-bootstrap-refresh-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        await secureStore.store(key: "session.accessToken", value: "expired-token")
        await secureStore.store(key: "session.refreshToken", value: "valid-refresh")

        let bootstrapService = FailingThenSucceedingBootstrapService(
            failWith: .unauthorized("Token expired"),
            failLoadCount: 1
        )
        let sessionStore = AppSessionStore(
            bootstrapService: bootstrapService,
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )

        await sessionStore.bootstrap()

        #expect(sessionStore.state.connectionStatus == .connected)
        #expect(sessionStore.launchState == .ready)
    }

    @Test @MainActor
    func sessionBootstrapForcesRegistrationWhenBothTokensExpired() async throws {
        let suiteName = "session-bootstrap-force-register-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        await secureStore.store(key: "session.accessToken", value: "expired-token")
        await secureStore.store(key: "session.refreshToken", value: "expired-refresh")

        let bootstrapService = AlwaysFailingBootstrapService(failWith: .unauthorized("Both tokens expired"))
        let sessionStore = AppSessionStore(
            bootstrapService: bootstrapService,
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )

        await sessionStore.bootstrap()

        // Should attempt registration after refresh fails
        #expect(bootstrapService.registerCallCount == 1)
    }

    @Test @MainActor
    func sessionBootstrapSetsAuthFailureStateWhenRecoveryImpossible() async throws {
        let suiteName = "session-bootstrap-auth-failure-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        await secureStore.store(key: "session.accessToken", value: "expired-token")
        await secureStore.store(key: "session.refreshToken", value: "expired-refresh")

        // Both loadSession and registerDevice fail with 401
        let bootstrapService = AlwaysFailingBootstrapService(failWith: .unauthorized("Registration rejected"))
        bootstrapService.alsoFailRegistration = true
        let sessionStore = AppSessionStore(
            bootstrapService: bootstrapService,
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )

        await sessionStore.bootstrap()

        #expect(sessionStore.launchState == .authFailure)
        #expect(sessionStore.state.connectionStatus == .error)
    }

    @Test @MainActor
    func sessionBootstrapSetsNetworkFailureStateForTimeout() async throws {
        let suiteName = "session-bootstrap-timeout-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        await secureStore.store(key: "session.accessToken", value: "valid-token")
        await secureStore.store(key: "session.refreshToken", value: "valid-refresh")

        let timeoutError = NSError(domain: NSURLErrorDomain, code: -1001, userInfo: nil) // timedOut
        let bootstrapService = AlwaysFailingBootstrapService(failWith: .requestFailed(timeoutError.localizedDescription))
        let sessionStore = AppSessionStore(
            bootstrapService: bootstrapService,
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )

        await sessionStore.bootstrap()

        if case .networkFailure = sessionStore.launchState {
            // Expected
        } else {
            Issue.record("Expected networkFailure state, got \(sessionStore.launchState)")
        }
    }
}

// MARK: - Test Doubles for Expired Token Recovery

@MainActor
private final class FailingThenSucceedingBootstrapService: SessionBootstrapServiceProtocol {
    private let failWith: RelayAPIClient.ClientError
    private var failLoadCount: Int
    private var loadAttempts = 0

    init(failWith: RelayAPIClient.ClientError, failLoadCount: Int) {
        self.failWith = failWith
        self.failLoadCount = failLoadCount
    }

    func registerDevice(_ request: DeviceRegistrationRequest) async throws -> SessionBootstrapResponse {
        SessionBootstrapResponse(
            state: AppSessionState(
                deviceID: UUID(),
                installationID: request.installationID,
                deviceRegistered: true,
                connectionStatus: .connected,
                syncStatus: .synced,
                isMockMode: false,
                backendEndpoint: request.relayBaseURLString,
                lastSyncAt: nil,
                pushTokenRegistered: false
            ),
            tokens: AuthTokens(
                accessToken: "new-access-token",
                refreshToken: "new-refresh-token",
                expiresAt: .distantFuture
            )
        )
    }

    func loadSession(accessToken: String?) async throws -> AppSessionState {
        loadAttempts += 1
        if loadAttempts <= failLoadCount {
            throw failWith
        }
        return AppSessionState(
            userID: UUID(),
            displayName: "Test User",
            deviceID: UUID(),
            installationID: UUID(),
            deviceRegistered: true,
            connectionStatus: .connected,
            syncStatus: .synced,
            isMockMode: false,
            backendEndpoint: "https://relay.example.com/v1",
            lastSyncAt: .now,
            pushTokenRegistered: false
        )
    }

    func refreshAuth(refreshToken: String) async throws -> AuthTokens {
        AuthTokens(
            accessToken: "refreshed-access-token",
            refreshToken: "refreshed-refresh-token",
            expiresAt: .distantFuture
        )
    }

    func revokeCurrentSession(accessToken: String?) async throws {}
}

@MainActor
private final class AlwaysFailingBootstrapService: SessionBootstrapServiceProtocol {
    private let failWith: RelayAPIClient.ClientError
    var registerCallCount = 0
    var alsoFailRegistration = false

    init(failWith: RelayAPIClient.ClientError) {
        self.failWith = failWith
    }

    func registerDevice(_ request: DeviceRegistrationRequest) async throws -> SessionBootstrapResponse {
        registerCallCount += 1
        if alsoFailRegistration {
            throw failWith
        }
        return SessionBootstrapResponse(
            state: AppSessionState(
                deviceID: UUID(),
                installationID: request.installationID,
                deviceRegistered: true,
                connectionStatus: .connected,
                syncStatus: .synced,
                isMockMode: false,
                backendEndpoint: request.relayBaseURLString,
                lastSyncAt: nil,
                pushTokenRegistered: false
            ),
            tokens: AuthTokens(
                accessToken: "new-access-token",
                refreshToken: "new-refresh-token",
                expiresAt: .distantFuture
            )
        )
    }

    func loadSession(accessToken: String?) async throws -> AppSessionState {
        throw failWith
    }

    func refreshAuth(refreshToken: String) async throws -> AuthTokens {
        throw failWith
    }

    func revokeCurrentSession(accessToken: String?) async throws {}
}

// MARK: - Notification Reply Cold-Launch Tests

@Suite(.serialized)
struct NotificationReplyTests {

    @Test @MainActor
    func pendingNotificationRoutePreservesReplyText() {
        let route = AppContainer.PendingNotificationRoute(
            conversationID: UUID(),
            messageID: "msg-123",
            jobID: "job-456",
            action: NotificationActionID.reply,
            replyText: "Hello from notification"
        )

        #expect(route.replyText == "Hello from notification")
        #expect(route.conversationID != nil)
        #expect(route.action == NotificationActionID.reply)
    }

    @Test @MainActor
    func pendingNotificationRouteStoresNilReplyText() {
        let route = AppContainer.PendingNotificationRoute(
            conversationID: UUID(),
            messageID: nil,
            jobID: nil,
            action: NotificationActionID.reply,
            replyText: nil
        )

        #expect(route.replyText == nil)
    }

    @Test @MainActor
    func pendingNotificationRoutePreservesReplyTextAcrossColdLaunch() {
        let conversationID = UUID()
        let expectedText = "Reply from lock screen"

        let route = AppContainer.PendingNotificationRoute(
            conversationID: conversationID,
            messageID: "msg-789",
            jobID: nil,
            action: NotificationActionID.reply,
            replyText: expectedText
        )

        #expect(route.replyText == expectedText)
        #expect(route.conversationID == conversationID)
    }

    // MARK: - Notes Navigation

    @Test("SidebarSection includes .notes case")
    func sidebarSectionIncludesNotes() {
        let allCases = SidebarSection.allCases
        #expect(allCases.contains(.notes))
    }

    @Test("SidebarSection .notes has correct title and icon")
    func notesSectionMetadata() {
        #expect(SidebarSection.notes.title == "Notes")
        #expect(SidebarSection.notes.icon == "pencil.and.outline")
    }

    @Test("NotesStore starts with empty state")
    @MainActor
    func notesStoreInitialState() {
        let store = NotesStore()
        #expect(store.notes.isEmpty)
        #expect(store.selectedNoteId == nil)
        #expect(store.isLoading == false)
        #expect(store.errorMessage == nil)
    }

    @Test("NotesStore activeNotes excludes deleted")
    @MainActor
    func notesStoreActiveExcludesDeleted() {
        let store = NotesStore()
        store.notes = [
            HeraldNote(title: "Active"),
            HeraldNote(title: "Deleted", deletedAt: .now),
        ]
        #expect(store.activeNotes.count == 1)
        #expect(store.activeNotes.first?.title == "Active")
    }

    @Test("NotesStore activeNotes sorts pinned first")
    @MainActor
    func notesStorePinnedFirst() {
        let store = NotesStore()
        store.notes = [
            HeraldNote(title: "Normal"),
            HeraldNote(title: "Pinned", pinned: true),
        ]
        #expect(store.activeNotes.first?.title == "Pinned")
    }

    // MARK: - Profile-Aware Failure Copy (B3)

    @Test("Failure copy uses active profile name")
    @MainActor
    func failureCopyUsesProfileName() {
        let heraldClient = MockHeraldClient()
        let suiteName = "failure-copy-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let profileStore = ProfileStore(apiClient: nil, accessTokenProvider: { nil })
        profileStore.profiles = [
            ProfileStore.HeraldProfile(name: "Atlas", description: "Test", skillCount: 0),
        ]
        profileStore.markActive("Atlas")

        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)
        chatStore.profileStore = profileStore

        let message = chatStore.failureMessage()

        #expect(message.contains("Atlas"))
        #expect(!message.contains("Herald"))
    }

    @Test("Failure copy falls back to Herald when no profile active")
    @MainActor
    func failureCopyFallsBackToHerald() {
        let heraldClient = MockHeraldClient()
        let suiteName = "failure-copy-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let profileStore = ProfileStore(apiClient: nil, accessTokenProvider: { nil })

        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)
        chatStore.profileStore = profileStore

        let message = chatStore.failureMessage()

        #expect(message.contains("Herald"))
    }

    // MARK: - B4: Chat Title Reliability

    @Test("User rename prevents auto-title from overwriting")
    @MainActor
    func titleOwnership_UserRenamePreventsAutoTitle() async throws {
        final class TitleTrackingClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?
            var generateTitleCallCount = 0

            func connect() async {}
            func disconnect() async {}
            func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "reply", status: .delivered)
            }
            func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: UUID()))
                        continuation.yield(.finished(Message(sender: .herald, content: "Reply", status: .delivered), nil, nil, nil))
                        continuation.finish()
                    }
                }
            }
            func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func clearConversation() async throws -> Conversation { Conversation(title: "Herald") }
            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Herald") }
            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
                generateTitleCallCount += 1
                return "Should Not Apply"
            }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
                Message(sender: .herald, content: "reply", status: .delivered)
            }
            func cancelJob(jobID: UUID) async throws {}
        }

        let client = TitleTrackingClient()
        let suiteName = "title-ownership-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let chatStore = ChatStore(heraldClient: client, persistence: persistence)

        chatStore.conversation = Conversation(title: "New Chat", messages: [
            Message(sender: .user, content: "Hello", status: .sent),
            Message(sender: .herald, content: "Hi there", status: .delivered),
        ])

        // User renames the conversation
        chatStore.setConversationTitle("My Custom Title")
        #expect(chatStore.conversation?.title == "My Custom Title")

        // Send a message through the streaming path to trigger autoTitleIfNeeded()
        await chatStore.sendMessage("Follow-up message")

        // Title should still be the user's custom title
        #expect(chatStore.conversation?.title == "My Custom Title")
        // generateSessionTitle should never have been called
        #expect(client.generateTitleCallCount == 0)
    }

    @Test("Title RPC failure uses deterministic local fallback")
    @MainActor
    func titleRPCFailure_UsesFallbackAndLogs() async throws {
        final class FailingTitleClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?
            var renameSessionCallCount = 0
            var lastRenamedTitle: String?

            func connect() async {}
            func disconnect() async {}
            func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "reply", status: .delivered)
            }
            func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: UUID()))
                        continuation.yield(.finished(Message(sender: .herald, content: "Reply", status: .delivered), nil, nil, nil))
                        continuation.finish()
                    }
                }
            }
            func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func clearConversation() async throws -> Conversation { Conversation(title: "Herald") }
            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Herald") }
            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary {
                renameSessionCallCount += 1
                lastRenamedTitle = title
                return SessionSummary(id: id, title: title)
            }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
                throw URLError(.timedOut)
            }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
                Message(sender: .herald, content: "reply", status: .delivered)
            }
            func cancelJob(jobID: UUID) async throws {}
        }

        let client = FailingTitleClient()
        let suiteName = "title-fallback-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let chatStore = ChatStore(heraldClient: client, persistence: persistence)

        let longMessage = "This is a very long first message that should be truncated for the title"
        chatStore.conversation = Conversation(title: "New Chat", messages: [
            Message(sender: .user, content: longMessage, status: .sent),
            Message(sender: .herald, content: "Some reply", status: .delivered),
        ])

        // Trigger autoTitleIfNeeded via the streaming path
        await chatStore.sendMessage("Another message")

        // The deterministic fallback should be the truncated first user message
        let expectedTitle = longMessage.count > 60 ? String(longMessage.prefix(57)) + "..." : longMessage
        #expect(chatStore.conversation?.title == expectedTitle)
        #expect(client.renameSessionCallCount == 1)
        #expect(client.lastRenamedTitle == expectedTitle)
    }

    @Test("Title RPC retries on failure and eventually succeeds")
    @MainActor
    func titleRPCRetriesAndSucceeds() async throws {
        final class RetryTitleClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?
            var generateTitleAttempts = 0

            func connect() async {}
            func disconnect() async {}
            func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "reply", status: .delivered)
            }
            func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: UUID()))
                        continuation.yield(.finished(Message(sender: .herald, content: "Reply", status: .delivered), nil, nil, nil))
                        continuation.finish()
                    }
                }
            }
            func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func clearConversation() async throws -> Conversation { Conversation(title: "Herald") }
            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Herald") }
            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
                generateTitleAttempts += 1
                if generateTitleAttempts < 2 {
                    throw URLError(.timedOut)
                }
                return "Recovered Title"
            }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
                Message(sender: .herald, content: "reply", status: .delivered)
            }
            func cancelJob(jobID: UUID) async throws {}
        }

        let client = RetryTitleClient()
        let suiteName = "title-retry-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let chatStore = ChatStore(heraldClient: client, persistence: persistence)

        chatStore.conversation = Conversation(title: "New Chat", messages: [
            Message(sender: .user, content: "Hello", status: .sent),
            Message(sender: .herald, content: "Hi there", status: .delivered),
        ])

        // Trigger autoTitleIfNeeded via the streaming path
        await chatStore.sendMessage("Follow-up")

        // Title should be the one returned by the second attempt
        #expect(chatStore.conversation?.title == "Recovered Title")
        // Both attempts should have been made
        #expect(client.generateTitleAttempts == 2)
    }

    @Test("Auto-generated title persists across relaunch")
    @MainActor
    func autoGeneratedTitlePersistsAcrossRelaunch() async throws {
        final class PersistTitleClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?

            func connect() async {}
            func disconnect() async {}
            func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID) async -> Message {
                Message(sender: .herald, content: "reply", status: .delivered)
            }
            func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
                AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: UUID()))
                        continuation.yield(.finished(Message(sender: .herald, content: "Reply", status: .delivered), nil, nil, nil))
                        continuation.finish()
                    }
                }
            }
            func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func clearConversation() async throws -> Conversation { Conversation(title: "Herald") }
            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Herald") }
            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
                "Persisted Title"
            }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
                Message(sender: .herald, content: "reply", status: .delivered)
            }
            func cancelJob(jobID: UUID) async throws {}
        }

        let suiteName = "title-persistence-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)

        // First "session" — create ChatStore, send message, trigger auto-title
        let client = PersistTitleClient()
        let chatStore = ChatStore(heraldClient: client, persistence: persistence)
        chatStore.conversation = Conversation(title: "New Chat", messages: [
            Message(sender: .user, content: "What is the meaning of life?", status: .sent),
            Message(sender: .herald, content: "42", status: .delivered),
        ])

        await chatStore.sendMessage("Tell me more")

        #expect(chatStore.conversation?.title == "Persisted Title")

        // Simulate relaunch: create a new ChatStore with the same persistence
        let relaunchedClient = PersistTitleClient()
        let relaunchedStore = ChatStore(heraldClient: relaunchedClient, persistence: persistence)
        await relaunchedStore.loadConversationIfNeeded()

        #expect(relaunchedStore.conversation?.title == "Persisted Title")
    }

    @Test("Failure copy falls back to Herald when profileStore is nil")
    @MainActor
    func failureCopyFallsBackWhenNoProfileStore() {
        let heraldClient = MockHeraldClient()
        let suiteName = "failure-copy-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)

        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        let message = chatStore.failureMessage()

        #expect(message.contains("Herald"))
    }

    // MARK: - ConnectionStatus Tests

    @Test("ConnectionStatus has all required states")
    func connectionStatusHasAllRequiredStates() {
        let disconnected = ConnectionStatus.disconnected
        let connecting = ConnectionStatus.connecting
        let connected = ConnectionStatus.connected
        let reconnecting = ConnectionStatus.reconnecting
        let degraded = ConnectionStatus.degraded
        let error = ConnectionStatus.error

        #expect(disconnected.displayLabel == "Disconnected")
        #expect(connecting.displayLabel == "Connecting...")
        #expect(connected.displayLabel == "Connected")
        #expect(reconnecting.displayLabel == "Reconnecting...")
        #expect(degraded.displayLabel == "Degraded")
        #expect(error.displayLabel == "Error")
    }

    @Test("ConnectionStatus dot colors match spec")
    func connectionStatusDotColorsMatchSpec() {
        #expect(ConnectionStatus.connected.dotColor == .green)
        #expect(ConnectionStatus.connecting.dotColor == .yellow)
        #expect(ConnectionStatus.reconnecting.dotColor == .yellow)
        #expect(ConnectionStatus.degraded.dotColor == .orange)
        #expect(ConnectionStatus.disconnected.dotColor == .gray)
        #expect(ConnectionStatus.error.dotColor == .gray)
    }

    @Test("ConnectionStatus display icons are set")
    func connectionStatusDisplayIconsAreSet() {
        #expect(ConnectionStatus.disconnected.displayIcon == "xmark.circle.fill")
        #expect(ConnectionStatus.connecting.displayIcon == "arrow.triangle.2.circlepath")
        #expect(ConnectionStatus.connected.displayIcon == "checkmark.circle.fill")
        #expect(ConnectionStatus.reconnecting.displayIcon == "arrow.triangle.2.circlepath")
        #expect(ConnectionStatus.degraded.displayIcon == "exclamationmark.triangle.fill")
        #expect(ConnectionStatus.error.displayIcon == "exclamationmark.circle.fill")
    }

    @Test("ConnectionStatus codable round trip")
    func connectionStatusCodableRoundTrip() throws {
        let statuses: [ConnectionStatus] = [.disconnected, .connecting, .connected, .reconnecting, .degraded, .error]
        for status in statuses {
            let data = try JSONEncoder().encode(status)
            let decoded = try JSONDecoder().decode(ConnectionStatus.self, from: data)
            #expect(decoded == status)
        }
    }

    @Test("ChatStore connectionStatus reflects heraldClient status")
    @MainActor
    func chatStoreConnectionStatusReflectsHeraldClient() {
        let heraldClient = MockHeraldClient()
        let suiteName = "connection-status-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)

        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        heraldClient.connectionStatus = .connected
        #expect(chatStore.connectionStatus == .connected)

        heraldClient.connectionStatus = .reconnecting
        #expect(chatStore.connectionStatus == .reconnecting)

        heraldClient.connectionStatus = .degraded
        #expect(chatStore.connectionStatus == .degraded)
    }

    @Test("ChatStore updateConnectionStatus propagates to heraldClient")
    @MainActor
    func chatStoreUpdateConnectionStatusPropagates() {
        let heraldClient = MockHeraldClient()
        let suiteName = "update-status-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)

        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        chatStore.updateConnectionStatus(.reconnecting)
        #expect(heraldClient.connectionStatus == .reconnecting)

        chatStore.updateConnectionStatus(.degraded)
        #expect(heraldClient.connectionStatus == .degraded)

        chatStore.updateConnectionStatus(.connected)
        #expect(heraldClient.connectionStatus == .connected)
    }
}
