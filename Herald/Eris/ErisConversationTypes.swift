import Foundation

// MARK: - Conversation wire types
//
// Shapes mirror `crates/eris-host/src/device/handlers.rs`
// (`list_conversations`, `create_conversation`, `conversation`,
// `clear_conversation`) and the rows they serialize out of
// `crates/eris-host/src/web/chat.rs` (`Conversation`, `Message`, `ToolRecord`).
//
// `updated_at` and `created_at` are the database stamps the daemon stores, not
// RFC 3339, so they stay `String` here: this app compares and displays them but
// never re-parses them into a `Date` that could come back nil.

// MARK: - List

/// One row of `GET /v1/conversations`.
struct ErisConversationSummary: Identifiable, Equatable, Sendable {
    /// Opaque id, also the transcript key.
    let id: String
    /// What the operator reads.
    let title: String
    /// The stamp of the last message. The daemon already ordered rows by it.
    let updatedAt: String
    /// The project this conversation is filed under, when it is filed.
    let projectID: String?
    /// How many messages the conversation holds.
    let messageCount: Int

    init(id: String, title: String, updatedAt: String, projectID: String?, messageCount: Int) {
        self.id = id
        self.title = title
        self.updatedAt = updatedAt
        self.projectID = projectID
        self.messageCount = messageCount
    }

    /// A title that is never an empty row.
    var displayTitle: String {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Untitled conversation" : trimmed
    }

    /// The message count as a phrase, because "1 messages" reads like a bug.
    var messageCountText: String {
        messageCount == 1 ? "1 message" : "\(messageCount) messages"
    }
}

extension ErisConversationSummary: Decodable {
    private enum CodingKeys: String, CodingKey {
        case id
        case title
        case updatedAt = "updated_at"
        case projectID = "project_id"
        case messageCount = "message_count"
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard
            let id = try container.decodeIfPresent(String.self, forKey: .id),
            !id.isEmpty
        else {
            throw ErisError.decoding("a conversation in the list carried no id")
        }
        self.init(
            id: id,
            title: try container.decodeIfPresent(String.self, forKey: .title) ?? "",
            updatedAt: try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? "",
            projectID: try container.decodeIfPresent(String.self, forKey: .projectID),
            messageCount: try container.decodeIfPresent(Int.self, forKey: .messageCount) ?? 0
        )
    }
}

/// The `GET /v1/conversations` body.
struct ErisConversationPage: Equatable, Sendable, Decodable {
    /// How many rows this page carries.
    let count: Int
    /// Whether the daemon cut the page, so the client knows it is not seeing all.
    let truncated: Bool
    /// The rows, newest first.
    let conversations: [ErisConversationSummary]

    init(count: Int, truncated: Bool, conversations: [ErisConversationSummary]) {
        self.count = count
        self.truncated = truncated
        self.conversations = conversations
    }

    private enum CodingKeys: String, CodingKey {
        case count
        case truncated
        case conversations
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let rows = try container.decodeIfPresent([ErisConversationSummary].self, forKey: .conversations) ?? []
        self.init(
            count: try container.decodeIfPresent(Int.self, forKey: .count) ?? rows.count,
            truncated: try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false,
            conversations: rows
        )
    }
}

/// The `201` body from `POST /v1/conversations`.
struct ErisCreatedConversation: Equatable, Sendable, Decodable {
    /// The new conversation's id, which is what a turn is filed under.
    let id: String
    /// The daemon's placeholder title (`New chat`).
    let title: String

    init(id: String, title: String) {
        self.id = id
        self.title = title
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case title
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard
            let id = try container.decodeIfPresent(String.self, forKey: .id),
            !id.isEmpty
        else {
            throw ErisError.decoding("the created conversation carried no id")
        }
        self.init(
            id: id,
            title: try container.decodeIfPresent(String.self, forKey: .title) ?? ""
        )
    }
}

/// The `POST /v1/conversations/{id}/clear` body.
struct ErisClearResult: Equatable, Sendable, Decodable {
    /// How many messages were dropped.
    let cleared: Int

    init(cleared: Int) {
        self.cleared = cleared
    }

    private enum CodingKeys: String, CodingKey {
        case cleared
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(cleared: try container.decodeIfPresent(Int.self, forKey: .cleared) ?? 0)
    }
}

// MARK: - Detail

/// One tool call a turn ran, exactly as the transcript keeps it.
///
/// `arguments` is the JSON the model sent and `output` is what the tool returned,
/// both already bounded by the daemon. `isError` is what makes a refusal visible
/// as a refusal rather than as an answer that quietly ignored it.
struct ErisToolCall: Equatable, Sendable, Decodable {
    /// The tool name.
    let name: String
    /// The arguments, as JSON.
    let arguments: String
    /// What the tool returned.
    let output: String
    /// Whether the tool reported a failure.
    let isError: Bool

    init(name: String, arguments: String, output: String, isError: Bool) {
        self.name = name
        self.arguments = arguments
        self.output = output
        self.isError = isError
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case arguments
        case output
        case isError = "is_error"
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            name: try container.decodeIfPresent(String.self, forKey: .name) ?? "",
            arguments: try container.decodeIfPresent(String.self, forKey: .arguments) ?? "",
            output: try container.decodeIfPresent(String.self, forKey: .output) ?? "",
            isError: try container.decodeIfPresent(Bool.self, forKey: .isError) ?? false
        )
    }
}

/// One transcript row of `GET /v1/conversations/{id}`.
struct ErisConversationMessage: Identifiable, Equatable, Sendable {
    /// Row id.
    let id: String
    /// `user` or `assistant`, as stored.
    let role: String
    /// What was said, so far.
    let body: String
    /// `complete`, `streaming` or `failed`.
    let state: String
    /// When the row was written, as the database stamps it.
    let createdAt: String
    /// The model's own reasoning, when the provider streamed any.
    let reasoning: String?
    /// The tool calls this turn ran, in order.
    let tools: [ErisToolCall]
    /// The id the sending client minted for this message, when it sent one.
    ///
    /// This is the only handle that can match the daemon's row to the row the
    /// app drew the instant the operator hit send. `id` cannot: the daemon mints
    /// its own row id, so the two are different values for the same message.
    /// `nil` for every row written before the daemon stored it, and for turns
    /// sent by the browser, which mints no such id.
    let clientMessageID: String?
    /// The turn both rows of this exchange belong to.
    ///
    /// Question and answer carry the same value, which is what lets a client
    /// group an answer with its reasoning instead of guessing from timestamps.
    /// `nil` on rows written before the daemon stamped it.
    let jobID: String?
    /// The files this row carries, as the daemon stored them.
    ///
    /// Absent on rows written before the daemon kept attachments and empty on a
    /// text-only turn, so both decode to `[]`. Each `id` is the same id the sending
    /// turn named in its `attachments` field, which is what lets a reloaded
    /// transcript show the file the operator attached instead of a bare bubble.
    let attachments: [ErisAttachment]

    init(
        id: String,
        role: String,
        body: String,
        state: String,
        createdAt: String,
        reasoning: String?,
        tools: [ErisToolCall],
        clientMessageID: String? = nil,
        jobID: String? = nil,
        attachments: [ErisAttachment] = []
    ) {
        self.id = id
        self.role = role
        self.body = body
        self.state = state
        self.createdAt = createdAt
        self.reasoning = reasoning
        self.tools = tools
        self.clientMessageID = clientMessageID
        self.jobID = jobID
        self.attachments = attachments
    }

    /// Whether the row is the operator's question.
    var isUser: Bool { role == "user" }

    /// Whether the turn behind this row was refused.
    var isFailed: Bool { state == "failed" }

    /// The reasoning text, when there is any worth a row.
    var displayReasoning: String? {
        guard let reasoning else { return nil }
        let trimmed = reasoning.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

extension ErisConversationMessage: Decodable {
    private enum CodingKeys: String, CodingKey {
        case id
        case role
        case body
        case state
        case createdAt = "created_at"
        case reasoning
        case tools
        case clientMessageID = "client_message_id"
        case jobID = "job_id"
        case attachments
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        // Empty strings are folded to nil: the daemon sends an explicit null, and
        // a blank id is the same absence with worse manners.
        self.init(
            id: try container.decodeIfPresent(String.self, forKey: .id) ?? UUID().uuidString,
            role: try container.decodeIfPresent(String.self, forKey: .role) ?? "",
            body: try container.decodeIfPresent(String.self, forKey: .body) ?? "",
            state: try container.decodeIfPresent(String.self, forKey: .state) ?? "complete",
            createdAt: try container.decodeIfPresent(String.self, forKey: .createdAt) ?? "",
            reasoning: try container.decodeIfPresent(String.self, forKey: .reasoning),
            tools: try container.decodeIfPresent([ErisToolCall].self, forKey: .tools) ?? [],
            clientMessageID: Self.nonEmpty(
                try container.decodeIfPresent(String.self, forKey: .clientMessageID)
            ),
            jobID: Self.nonEmpty(try container.decodeIfPresent(String.self, forKey: .jobID)),
            attachments: Self.decodeAttachments(container)
        )
    }

    /// The row attachments, or none.
    ///
    /// Decoded tolerantly on purpose: one attachment the daemon reshaped must not
    /// cost the operator the whole transcript. A row whose attachments cannot be
    /// read still renders its text, just without the file.
    private static func decodeAttachments(
        _ container: KeyedDecodingContainer<CodingKeys>
    ) -> [ErisAttachment] {
        (try? container.decodeIfPresent([ErisAttachment].self, forKey: .attachments)) ?? []
    }

    private static func nonEmpty(_ value: String?) -> String? {
        guard let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }
        return value
    }
}

/// The `GET /v1/conversations/{id}` body: the conversation and its transcript.
struct ErisConversationDetail: Equatable, Sendable, Decodable {
    /// Opaque id.
    let id: String
    /// The stored title.
    let title: String
    /// The transcript, oldest first.
    let messages: [ErisConversationMessage]
    /// Whether the daemon bounded the page, so older rows are not in this body.
    let truncated: Bool

    init(id: String, title: String, messages: [ErisConversationMessage], truncated: Bool) {
        self.id = id
        self.title = title
        self.messages = messages
        self.truncated = truncated
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case title
        case messages
        case truncated
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard
            let id = try container.decodeIfPresent(String.self, forKey: .id),
            !id.isEmpty
        else {
            throw ErisError.decoding("the conversation carried no id")
        }
        self.init(
            id: id,
            title: try container.decodeIfPresent(String.self, forKey: .title) ?? "",
            messages: try container.decodeIfPresent([ErisConversationMessage].self, forKey: .messages) ?? [],
            truncated: try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        )
    }

    /// A title that is never an empty row.
    var displayTitle: String {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Untitled conversation" : trimmed
    }
}

// MARK: - Projects

/// One project a conversation can be filed under.
///
/// The conversation list carries a `project_id` only, so this is what turns that
/// id into the name a person reads.
struct ErisProjectSummary: Identifiable, Equatable, Sendable, Decodable {
    /// Opaque id, the value `project_id` holds.
    let id: String
    /// What the operator reads.
    let name: String

    init(id: String, name: String) {
        self.id = id
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case name
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            id: try container.decodeIfPresent(String.self, forKey: .id) ?? "",
            name: try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        )
    }
}

/// The `GET /v1/projects` body.
struct ErisProjectList: Equatable, Sendable, Decodable {
    /// Every project the daemon holds.
    let projects: [ErisProjectSummary]

    init(projects: [ErisProjectSummary]) {
        self.projects = projects
    }

    private enum CodingKeys: String, CodingKey {
        case projects
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(projects: try container.decodeIfPresent([ErisProjectSummary].self, forKey: .projects) ?? [])
    }

    /// The names, keyed by id, for a list row to resolve its own `project_id`.
    var namesByID: [String: String] {
        var names: [String: String] = [:]
        for project in projects where !project.id.isEmpty {
            names[project.id] = project.name
        }
        return names
    }
}
