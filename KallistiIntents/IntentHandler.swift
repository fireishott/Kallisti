import Intents
import os

private let logger = Logger(subsystem: "net.fihonline.herald", category: "Intents")

class IntentHandler: INExtension, INSendMessageIntentHandling, INSearchForMessagesIntentHandling, INSetMessageAttributeIntentHandling {

    override func handler(for intent: INIntent) -> Any {
        return self
    }

    // MARK: - INSendMessageIntentHandling

    func resolveRecipients(for intent: INSendMessageIntent, with completion: @escaping @Sendable ([INSendMessageRecipientResolutionResult]) -> Void) {
        if let conversationId = intent.conversationIdentifier, !conversationId.isEmpty {
            let recipient = INPerson(
                personHandle: INPersonHandle(value: "herald", type: .unknown),
                nameComponents: nil,
                displayName: "Herald",
                image: nil,
                contactIdentifier: nil,
                customIdentifier: conversationId
            )
            completion([INSendMessageRecipientResolutionResult.success(with: recipient)])
        } else {
            let herald = INPerson(
                personHandle: INPersonHandle(value: "herald", type: .unknown),
                nameComponents: nil,
                displayName: "Herald",
                image: nil,
                contactIdentifier: nil,
                customIdentifier: nil
            )
            completion([INSendMessageRecipientResolutionResult.success(with: herald)])
        }
    }

    func resolveContent(for intent: INSendMessageIntent, with completion: @escaping @Sendable (INStringResolutionResult) -> Void) {
        if let content = intent.content, !content.isEmpty {
            completion(.success(with: content))
        } else {
            completion(.needsValue())
        }
    }

    func confirm(intent: INSendMessageIntent, completion: @escaping @Sendable (INSendMessageIntentResponse) -> Void) {
        completion(INSendMessageIntentResponse(code: .ready, userActivity: nil))
    }

    func handle(intent: INSendMessageIntent, completion: @escaping @Sendable (INSendMessageIntentResponse) -> Void) {
        let content = intent.content ?? ""
        let conversationId = intent.conversationIdentifier

        Task {
            do {
                try await HeraldIntentAPI.sendMessage(content: content, conversationId: conversationId)
                let response = INSendMessageIntentResponse(code: .success, userActivity: nil)
                completion(response)
            } catch {
                logger.error("Failed to send message: \(error.localizedDescription)")
                let response = INSendMessageIntentResponse(code: .failure, userActivity: nil)
                completion(response)
            }
        }
    }

    // MARK: - INSearchForMessagesIntentHandling

    func handle(intent: INSearchForMessagesIntent, completion: @escaping @Sendable (INSearchForMessagesIntentResponse) -> Void) {
        Task {
            do {
                let messages = try await HeraldIntentAPI.fetchRecentMessages(limit: 20)
                let inMessages = messages.map { msg in
                    INMessage(
                        identifier: msg.id,
                        conversationIdentifier: msg.conversationId,
                        content: msg.content,
                        dateSent: msg.dateSent,
                        sender: INPerson(
                            personHandle: INPersonHandle(value: "herald", type: .unknown),
                            nameComponents: nil,
                            displayName: "Herald",
                            image: nil,
                            contactIdentifier: nil,
                            customIdentifier: nil
                        ),
                        recipients: [],
                        messageType: .text
                    )
                }
                let response = INSearchForMessagesIntentResponse(code: .success, userActivity: nil)
                response.messages = inMessages
                completion(response)
            } catch {
                logger.error("Failed to fetch messages: \(error.localizedDescription)")
                let response = INSearchForMessagesIntentResponse(code: .failure, userActivity: nil)
                completion(response)
            }
        }
    }

    // MARK: - INSetMessageAttributeIntentHandling

    func resolveAttribute(for intent: INSetMessageAttributeIntent, with completion: @escaping @Sendable (INMessageAttributeResolutionResult) -> Void) {
        completion(.success(with: intent.attribute))
    }

    func handle(intent: INSetMessageAttributeIntent, completion: @escaping @Sendable (INSetMessageAttributeIntentResponse) -> Void) {
        // Mark messages as read/unread — acknowledge without server call since
        // Herald does not track read state server-side.
        completion(INSetMessageAttributeIntentResponse(code: .success, userActivity: nil))
    }
}

// MARK: - Lightweight Relay API Client for Intents Extension

private enum HeraldIntentAPI {
    struct IntentMessage {
        let id: String
        let conversationId: String?
        let content: String
        let dateSent: Date?
    }

    private static let appGroupID = "group.net.fihonline.herald"
    private static let keychainService = "net.fihonline.herald.session"
    private static let accessTokenKey = "session.accessToken"

    private static func baseURL() throws -> String {
        guard let defaults = UserDefaults(suiteName: appGroupID) else {
            throw IntentAPIError.configurationMissing
        }
        let dataKey = "herald.widget.data"
        guard let data = defaults.data(forKey: dataKey),
              let decoded = try? JSONDecoder().decode(WidgetData.self, from: data),
              let baseURL = decoded.relayBaseURL,
              !baseURL.isEmpty else {
            throw IntentAPIError.configurationMissing
        }
        return baseURL
    }

    private static func accessToken() throws -> String {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: keychainService,
            kSecAttrAccount as String: accessTokenKey,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data else {
            throw IntentAPIError.unauthorized
        }
        return String(data: data, encoding: .utf8) ?? ""
    }

    private static func makeRequest(path: String, method: String, body: Data? = nil) throws -> URLRequest {
        let base = try baseURL().trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let cleanPath = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard let url = URL(string: "\(base)/\(cleanPath)") else {
            throw IntentAPIError.invalidURL
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = body
        let token = try accessToken()
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return request
    }

    static func sendMessage(content: String, conversationId: String?) async throws {
        struct Body: Encodable {
            let conversationId: String?
            let text: String
            let clientMessageId: String
        }
        let body = Body(
            conversationId: conversationId,
            text: content,
            clientMessageId: UUID().uuidString
        )
        let encoder = JSONEncoder()
        let requestData = try encoder.encode(body)
        let request = try makeRequest(path: "messages", method: "POST", body: requestData)
        let (_, response) = try await URLSession.shared.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse,
              (200 ..< 300).contains(httpResponse.statusCode) else {
            throw IntentAPIError.serverError
        }
    }

    static func fetchRecentMessages(limit: Int) async throws -> [IntentMessage] {
        struct ConversationData: Decodable {
            let conversation: ConversationPayload
        }
        struct ConversationPayload: Decodable {
            let id: String
            let messages: [MessagePayload]
        }
        struct MessagePayload: Decodable {
            let id: String
            let role: String
            let text: String
            let timestamp: Date?
        }

        let request = try makeRequest(path: "conversations/current", method: "GET")
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse,
              (200 ..< 300).contains(httpResponse.statusCode) else {
            throw IntentAPIError.serverError
        }
        // Relay wraps responses in { "data": { ... } }
        struct Envelope<T: Decodable>: Decodable { let data: T }
        let decoded = try decoder.decode(Envelope<ConversationData>.self, from: data)
        let conversationId = decoded.data.conversation.id
        return decoded.data.conversation.messages
            .filter { $0.role == "assistant" }
            .suffix(limit)
            .map { msg in
                IntentMessage(
                    id: msg.id,
                    conversationId: conversationId,
                    content: msg.text,
                    dateSent: msg.timestamp
                )
            }
    }

    enum IntentAPIError: Error {
        case configurationMissing
        case unauthorized
        case invalidURL
        case serverError
    }
}

// MARK: - Widget Data Model (subset needed for relay URL)

private struct WidgetData: Decodable {
    let relayBaseURL: String?
}
