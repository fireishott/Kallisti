import Foundation

/// A lightweight attachment reference stored on a message for display.
struct MessageAttachment: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let kind: String       // "image" or "file"
    let fileName: String
    let mimeType: String
    /// Base64-encoded thumbnail (for images) — small enough to cache/persist.
    let thumbnailBase64: String?
    let localStoragePath: String?
    /// Native-gateway media URL (e.g. `/v1/native/media?path=images/foo.png`)
    /// for history-loaded attachments. Fetched with the native bearer token;
    /// mutually exclusive with the relay `messageID`/`remoteIndex` path.
    let mediaURL: URL?
    /// Server message this attachment belongs to, and its position in that
    /// message's attachment list. Together they address the relay endpoint
    /// `messages/{messageID}/attachments/{remoteIndex}` for full-resolution
    /// bytes. Nil for locally-composed (not-yet-sent) attachments.
    let messageID: UUID?
    let remoteIndex: Int?

    var isImage: Bool { kind == "image" || mimeType.hasPrefix("image/") }

    /// True for types that render as an inline thumbnail (image, video, PDF)
    /// rather than a tap-to-open file card.
    var isInlinePreview: Bool {
        kind == "image" || kind == "video" || kind == "pdf" || mimeType.hasPrefix("image/")
    }

    init(
        id: UUID = UUID(),
        kind: String,
        fileName: String,
        mimeType: String,
        thumbnailBase64: String? = nil,
        localStoragePath: String? = nil,
        mediaURL: URL? = nil,
        messageID: UUID? = nil,
        remoteIndex: Int? = nil
    ) {
        self.id = id
        self.kind = kind
        self.fileName = fileName
        self.mimeType = mimeType
        self.thumbnailBase64 = thumbnailBase64
        self.localStoragePath = localStoragePath
        self.mediaURL = mediaURL
        self.messageID = messageID
        self.remoteIndex = remoteIndex
    }

    init(from pending: PendingAttachment) {
        self.id = pending.id
        self.kind = pending.kind.rawValue
        self.fileName = pending.fileName
        self.mimeType = pending.mimeType
        self.thumbnailBase64 = pending.thumbnailBase64
        self.localStoragePath = pending.localStoragePath
        self.mediaURL = nil
        self.messageID = nil
        self.remoteIndex = nil
    }
}

/// Which way the daemon sent a turn, as its `turn.start` frame reports it.
///
/// Every field is optional except the intent, and the whole route is optional
/// on the row: a daemon that does not route turns sends no `route` at all, and
/// one that routes may name only the intent. A `route` object with nothing a
/// badge could show decodes to `nil`, so a bubble never draws an empty chip.
struct TurnRoute: Equatable, Hashable, Sendable, Codable {
    /// `general`, `coding` or `heavy`. Empty when the daemon sent only a
    /// harness, which is still a route worth showing.
    let intent: String
    /// The harness the turn was handed to, when one was chosen.
    let harness: String?
    /// The model the turn resolved to, when the daemon named one.
    let model: String?
    /// Why the daemon routed it there, for a reader who taps through.
    let reason: String?

    init(intent: String = "", harness: String? = nil, model: String? = nil, reason: String? = nil) {
        self.intent = intent
        self.harness = harness
        self.model = model
        self.reason = reason
    }

    /// Read one `route` object off a `turn.start` frame.
    ///
    /// The frame is additive on the daemon's side, so this reads the keys it
    /// knows and ignores the rest rather than failing a turn over a field this
    /// build has not heard of.
    init?(json: Any?) {
        guard let object = json as? [String: Any] else { return nil }
        let intent = Self.text(object["intent"]) ?? ""
        let harness = Self.text(object["harness"])
        let model = Self.text(object["model"])
        let reason = Self.text(object["reason"])
        // With neither an intent nor a harness there is nothing to put on the
        // badge, which makes this "no route" rather than an empty one.
        guard !intent.isEmpty || harness != nil else { return nil }
        self.init(intent: intent, harness: harness, model: model, reason: reason)
    }

    private static func text(_ raw: Any?) -> String? {
        guard let value = raw as? String else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    /// The compact badge: `CODING · dsh`, `HEAVY`, or `dsh` for a general turn
    /// the daemon still sent through a harness.
    ///
    /// A general turn with no harness is nothing at all - the general model is
    /// the story there, and it already has its own line in Settings.
    var badgeText: String? {
        let intentTag: String?
        switch intent {
        case "coding": intentTag = "CODING"
        case "heavy": intentTag = "HEAVY"
        default: intentTag = nil
        }
        switch (intentTag, harness) {
        case (.some(let tag), .some(let harness)): return "\(tag) · \(harness)"
        case (.some(let tag), .none): return tag
        case (.none, .some(let harness)): return harness
        case (.none, .none): return nil
        }
    }
}

struct Message: Codable, Identifiable, Hashable, Sendable {
    /// Mutable so a transcript refresh can keep the id of the live streaming
    /// placeholder it matched by jobID. The stream targets that id on every
    /// frame; adopting the server's row id mid-turn silently dropped every
    /// delta after the first load.
    var id: UUID
    let clientMessageID: UUID?
    let sender: MessageSender
    var content: String
    /// Mutable so ChatStore.mergeConversationMetadata can re-assert the
    /// locally-known send time for a user message when a server refresh
    /// would otherwise overwrite it with `createdAt` (processing time).
    var timestamp: Date
    /// The relay job ID that produced this message. Mutable so the streaming
    /// placeholder (initialised nil) can acquire its job identity when
    /// `.messageSent` arrives — without this, the conversation refresh merge
    /// can't match a persisted assistant row to the live placeholder by jobID.
    var jobID: UUID?
    var status: MessageStatus
    var toolActivity: String?
    /// The daemon's id for this row, when it came from one. `id` is a UUID derived
    /// from it and cannot be reversed, so anything the daemon has to look up -
    /// paging, for one - sends this.
    var erisID: String?
    var toolActivities: [ToolActivity]
    var codeDiff: CodeDiff?
    /// A page a turn wrote (turn.artifact.page), rendered in a capped web view.
    var pageHTML: String?
    /// The height the daemon capped the page at, when it said.
    var pageHeightCap: Int?
    var isStreaming: Bool
    var voiceSessionDuration: TimeInterval?
    var attachments: [MessageAttachment]
    /// Streamed chain-of-thought / reasoning text, shown dimmed while the answer
    /// is being generated and collapsed to a summary once it completes.
    var reasoning: String
    /// How long reasoning streamed for, in seconds — used for the collapsed
    /// "Thought for Xs" label. Set when the final answer arrives.
    var reasoningDuration: TimeInterval?
    /// Error category from the connector (e.g. "context_exceeded", "timeout").
    /// Non-persisted — only set for in-memory failed messages.
    var errorCategory: String?
    /// Which way the daemon routed this turn, from its `turn.start` frame.
    /// Absent for a daemon that does not route, for a turn this device only
    /// reattached to (the frame is behind the tail), and for every row the
    /// transcript alone can describe.
    var route: TurnRoute?

    /// Whether this message was transcribed from a voice session.
    var isVoiceTranscript: Bool {
        sender == .voiceUser || sender == .voiceHerald
    }

    /// Composite ID that changes when content length changes during streaming.
    /// Used by LazyVStack to force layout recalculation as streaming text grows,
    /// preventing the "content expands but scroll doesn't track" issue.
    var streamingCompositeID: String {
        if isStreaming {
            return "\(id.uuidString)-c\(content.count)-r\(reasoning.count)"
        }
        return id.uuidString
    }

    init(
        id: UUID = UUID(),
        clientMessageID: UUID? = nil,
        sender: MessageSender,
        content: String,
        timestamp: Date = .now,
        jobID: UUID? = nil,
        status: MessageStatus = .sent,
        toolActivity: String? = nil,
        toolActivities: [ToolActivity] = [],
        codeDiff: CodeDiff? = nil,
        isStreaming: Bool = false,
        voiceSessionDuration: TimeInterval? = nil,
        attachments: [MessageAttachment] = [],
        reasoning: String = "",
        reasoningDuration: TimeInterval? = nil,
        errorCategory: String? = nil,
        erisID: String? = nil,
        route: TurnRoute? = nil
    ) {
        self.id = id
        self.clientMessageID = clientMessageID
        self.sender = sender
        self.content = content
        self.timestamp = timestamp
        self.jobID = jobID
        self.status = status
        self.toolActivity = toolActivity
        self.toolActivities = toolActivities
        self.codeDiff = codeDiff
        self.isStreaming = isStreaming
        self.voiceSessionDuration = voiceSessionDuration
        self.attachments = attachments
        self.reasoning = reasoning
        self.reasoningDuration = reasoningDuration
        self.errorCategory = errorCategory
        self.erisID = erisID
        self.route = route
    }

    enum CodingKeys: String, CodingKey {
        case id, clientMessageID, sender, content, timestamp, jobID, status, attachments
        case reasoning, reasoningDuration
        case toolActivities
        case codeDiff
        case route
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        clientMessageID = try container.decodeIfPresent(UUID.self, forKey: .clientMessageID)
        sender = try container.decode(MessageSender.self, forKey: .sender)
        content = try container.decode(String.self, forKey: .content)
        timestamp = try container.decode(Date.self, forKey: .timestamp)
        jobID = try container.decodeIfPresent(UUID.self, forKey: .jobID)
        status = try container.decode(MessageStatus.self, forKey: .status)
        attachments = try container.decodeIfPresent([MessageAttachment].self, forKey: .attachments) ?? []
        reasoning = try container.decodeIfPresent(String.self, forKey: .reasoning) ?? ""
        reasoningDuration = try container.decodeIfPresent(TimeInterval.self, forKey: .reasoningDuration)
        toolActivities = try container.decodeIfPresent([ToolActivity].self, forKey: .toolActivities) ?? []
        toolActivity = nil
        codeDiff = try container.decodeIfPresent(CodeDiff.self, forKey: .codeDiff)
        route = try container.decodeIfPresent(TurnRoute.self, forKey: .route)
        isStreaming = false
        voiceSessionDuration = nil
        errorCategory = nil
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encodeIfPresent(clientMessageID, forKey: .clientMessageID)
        try container.encode(sender, forKey: .sender)
        try container.encode(content, forKey: .content)
        try container.encode(timestamp, forKey: .timestamp)
        try container.encodeIfPresent(jobID, forKey: .jobID)
        try container.encode(status, forKey: .status)
        if !attachments.isEmpty {
            try container.encode(attachments, forKey: .attachments)
        }
        if !reasoning.isEmpty {
            try container.encode(reasoning, forKey: .reasoning)
        }
        try container.encodeIfPresent(reasoningDuration, forKey: .reasoningDuration)
        if !toolActivities.isEmpty {
            try container.encode(toolActivities, forKey: .toolActivities)
        }
        if let codeDiff {
            try container.encode(codeDiff, forKey: .codeDiff)
        }
        try container.encodeIfPresent(route, forKey: .route)
    }
}
