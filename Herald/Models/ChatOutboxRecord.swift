import CryptoKit
import Foundation

// MARK: - Durable outbox record (Build 33 Workstream B)

/// Durable outbox record: a user message that must survive force-quit and
/// conversation switches. The manifest (a JSON array of these) lives in
/// Application Support; attachment BYTES are staged next to it and referenced
/// by stableID + relative path — never embedded in the manifest.
///
/// Lifecycle: `.queued` → `.materializing` (leased for submit) → `.submitting`
/// (POST in flight) → `.accepted` (relay took the job, jobID known) →
/// `.terminal` (canonical IDs recorded). Failures land in
/// `.retryableFailure` (with exponential backoff) or `.permanentFailure`.
struct ChatOutboxRecord: Codable, Identifiable, Sendable {
    let schemaVersion: Int
    let clientMessageID: UUID
    let conversationID: UUID
    let createdAt: Date
    /// Monotonically increasing FIFO order, assigned from the manifest's
    /// `nextSequence` counter at enqueue time.
    let sequence: Int
    var cleanText: String
    let continuationContext: String?
    var attachmentRefs: [OutboxAttachmentRef]
    var state: OutboxItemState
    /// Relay-assigned job ID once the message is accepted.
    var jobID: UUID?
    /// The canonical user message row id. Set at enqueue (the optimistic row
    /// IS the canonical row, addressed by clientMessageID) and preserved
    /// through terminal.
    var canonicalUserMessageID: UUID?
    /// The terminal assistant message id once the turn completes.
    var terminalMessageID: UUID?
    var attemptCount: Int
    /// When a retryable failure may be auto-submitted again. nil = manual
    /// retry only.
    var nextAttemptAt: Date?
    var lastError: String?

    // MARK: Build 102 P0-D — correlation envelope
    //
    // Every logical send carries one stable envelope visible in diagnostics,
    // persisted across restart, and echoed by the connector. Required by
    // marching orders §8 so a single optimistic row, outbox record, request,
    // job, terminal assistant response, and real Hermes session can be
    // correlated from any single one of those handles.

    /// Stable per-install identifier (Pair-Setup token hash). Set at enqueue
    /// from the live HeraldClient; nil for tests that don't inject one.
    var installationId: String?
    /// Physical device id (e.g. APNs device token or local UUID). Distinct
    /// from installationId so multi-device accounts can be reconciled.
    var deviceId: String?
    /// Real Hermes session id once the binding has been resolved by
    /// /v1/conversations/ensure or _bind_conversation_early. NEVER fabricated
    /// — this is the canonical session, validated against the agent.
    var hermesSessionId: String?
    /// Server-assigned request id (delivery_store.message_requests row PK).
    /// Echoed by the connector so we can join a local outbox record to the
    /// authoritative server-side request row.
    var requestId: String?
    /// UUID for THIS attempt. Distinct from `attemptCount` so multiple retries
    /// of the same clientMessageID each get a unique handle; the active
    /// attempt's id appears in structured logs and the diagnostics detail
    /// sheet per marching orders §8.
    var attemptID: UUID?
    /// Last state-transition timestamp. Updated on every state change
    /// (queued → materializing → submitting → accepted → terminal). Used by
    /// the diagnostics sheet to show the most recent event time.
    var updatedAt: Date

    var id: UUID { clientMessageID }

    var isInFlight: Bool {
        state == .materializing || state == .submitting || state == .accepted
    }

    var isTerminal: Bool {
        state == .terminal || state == .permanentFailure || state == .cancelled
    }
}

/// Reference to a staged attachment. The bytes live at
/// Application Support/Herald/AttachmentStaging/<conversationID>/<relativePath>.
/// `relativePath` encodes the original file name after a stableID prefix so
/// the submit path can rebuild a PendingAttachment without extra metadata.
struct OutboxAttachmentRef: Codable, Sendable {
    let stableID: UUID
    let relativePath: String
    let mimeType: String
    let byteLength: Int
    let sha256: String
}

enum OutboxItemState: String, Codable, Sendable {
    case drafted
    case queued
    case materializing
    case submitting
    case accepted
    case terminal
    case retryableFailure
    case permanentFailure
    case cancelled
}

/// Whole manifest file: items plus the FIFO sequence counter.
struct ChatOutboxManifest: Codable, Sendable {
    var schemaVersion: Int
    var items: [ChatOutboxRecord]
    var nextSequence: Int
}

// MARK: - Send phase (Build 33 Workstream B)

/// Full send lifecycle for the durable outbox, parallel to `StreamingPhase`.
/// UI-facing; each phase belongs to the immutable
/// (conversationID, clientMessageID, attemptID) tuple so a phase from a
/// superseded attempt can never overwrite the active banner.
enum MessageSendPhase: Sendable, Equatable {
    case idle
    case preparing            // Validating, staging attachments
    case checkingHermes       // ensureConversation in flight
    case creatingConversation // New session materializing
    case queued               // Waiting behind another active job
    case submitting           // POST in flight
    case waitingForHermes     // Job accepted, waiting for first token
    case streaming            // Receiving deltas
    case restoringStream      // Reconnecting after drop
    case completed            // Terminal success
    case failed(String)       // Terminal failure with reason
}

/// The immutable identity a `sendPhase` belongs to. Bumped on every lease so
/// stale updates from a superseded attempt cannot clobber the active phase.
struct SendPhaseOwner: Sendable, Equatable {
    let conversationID: UUID
    let clientMessageID: UUID
    let attemptID: UUID
}

// MARK: - Manifest + staging persistence

/// File-backed store for the durable chat outbox and its staged attachment
/// bytes. Atomic writes (temp file + rename) so a crash mid-save can never
/// corrupt the manifest. Pure file I/O — callers (ChatStore) are MainActor.
struct OutboxManifestStore: Sendable {
    static let schemaVersion = 1

    let manifestURL: URL
    let stagingRootURL: URL

    init(baseDirectory: URL? = nil) {
        let base = baseDirectory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        manifestURL = base
            .appendingPathComponent("Kallisti", isDirectory: true)
            .appendingPathComponent("Outbox", isDirectory: true)
            .appendingPathComponent("outbox.json", isDirectory: false)
        stagingRootURL = base
            .appendingPathComponent("Kallisti", isDirectory: true)
            .appendingPathComponent("AttachmentStaging", isDirectory: true)
    }

    // MARK: Manifest

    func load() -> ChatOutboxManifest {
        guard let data = try? Data(contentsOf: manifestURL) else {
            return ChatOutboxManifest(schemaVersion: Self.schemaVersion, items: [], nextSequence: 0)
        }
        guard let manifest = try? JSONDecoder().decode(ChatOutboxManifest.self, from: data) else {
            // Corrupt manifest. Start fresh rather than crash or — worse —
            // auto-resend items in unknown states. Items are conversation-
            // scoped and idempotent by clientMessageID, so any orphaned server
            // jobs reconcile on the next conversation load.
            return ChatOutboxManifest(schemaVersion: Self.schemaVersion, items: [], nextSequence: 0)
        }
        return manifest
    }

    func save(_ manifest: ChatOutboxManifest) {
        let directory = manifestURL.deletingLastPathComponent()
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let data = try JSONEncoder().encode(manifest)
            // Atomic write: temp file + rename so a crash mid-save never
            // leaves a truncated manifest that recovery would misread.
            let tempURL = directory.appendingPathComponent("outbox.json.tmp-\(UUID().uuidString)")
            try data.write(to: tempURL, options: .atomic)
            if FileManager.default.fileExists(atPath: manifestURL.path) {
                _ = try FileManager.default.replaceItemAt(manifestURL, withItemAt: tempURL)
            } else {
                try FileManager.default.moveItem(at: tempURL, to: manifestURL)
            }
        } catch {
            // Non-fatal: the in-memory outbox still works for this session;
            // the next successful save recovers durability.
        }
    }

    // MARK: Attachment staging

    /// Stage attachment bytes for an outbox item and return a reference.
    /// Thumbnail data (images) is written as a `<stableID>-thumb.jpg`
    /// sidecar so the manifest itself never carries bytes.
    func stageAttachment(
        conversationID: UUID,
        pending: PendingAttachment,
        stableID: UUID
    ) -> OutboxAttachmentRef? {
        let directory = stagingRootURL.appendingPathComponent(conversationID.uuidString, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let sanitized = Self.sanitizeFileName(pending.fileName)
            let relativePath = "\(stableID.uuidString)-\(sanitized)"
            try pending.data.write(to: directory.appendingPathComponent(relativePath), options: .atomic)
            if let thumbnail = pending.thumbnailData {
                try? thumbnail.write(
                    to: directory.appendingPathComponent("\(stableID.uuidString)-thumb.jpg"),
                    options: .atomic
                )
            }
            let digest = SHA256.hash(data: pending.data)
                .map { String(format: "%02x", $0) }
                .joined()
            return OutboxAttachmentRef(
                stableID: stableID,
                relativePath: relativePath,
                mimeType: pending.mimeType,
                byteLength: pending.data.count,
                sha256: digest
            )
        } catch {
            return nil
        }
    }

    /// Rebuild a PendingAttachment from a staged reference. Returns nil when
    /// the staged bytes are gone (staging directory was cleared) — the
    /// submit path must never fabricate empty attachments.
    func pendingAttachment(for ref: OutboxAttachmentRef, conversationID: UUID) -> PendingAttachment? {
        let directory = stagingRootURL.appendingPathComponent(conversationID.uuidString, isDirectory: true)
        let fileURL = directory.appendingPathComponent(ref.relativePath)
        guard let data = try? Data(contentsOf: fileURL) else { return nil }
        let thumbData = try? Data(contentsOf: directory.appendingPathComponent("\(ref.stableID.uuidString)-thumb.jpg"))
        return PendingAttachment(
            kind: ref.mimeType.hasPrefix("image/") ? .image : .file,
            fileName: Self.fileName(from: ref.relativePath),
            mimeType: ref.mimeType,
            data: data,
            localStoragePath: fileURL.path,
            thumbnailData: thumbData
        )
    }

    /// Best-effort cleanup of staged bytes for a terminal/cancelled item.
    func removeStagedAttachments(for item: ChatOutboxRecord) {
        let directory = stagingRootURL.appendingPathComponent(item.conversationID.uuidString, isDirectory: true)
        for ref in item.attachmentRefs {
            try? FileManager.default.removeItem(at: directory.appendingPathComponent(ref.relativePath))
            try? FileManager.default.removeItem(at: directory.appendingPathComponent("\(ref.stableID.uuidString)-thumb.jpg"))
        }
    }

    // MARK: Helpers

    static func sanitizeFileName(_ fileName: String) -> String {
        let invalidCharacters = CharacterSet(charactersIn: "/:\\?%*|\"<>")
        let cleaned = fileName.components(separatedBy: invalidCharacters).joined(separator: "_")
        return cleaned.isEmpty ? "attachment" : cleaned
    }

    /// Strip the `<stableID>-` prefix from a staged relativePath to recover
    /// the original file name.
    static func fileName(from relativePath: String) -> String {
        if let firstDash = relativePath.firstIndex(of: "-") {
            return String(relativePath[relativePath.index(after: firstDash)...])
        }
        return relativePath
    }
}

// MARK: - Per-conversation FIFO lease (Build 102 P0-A)

// Per-conversation FIFO lease held by the active submit attempt.
//
// Replaces the legacy global `submitInFlight: Bool`. Keyed by conversationID
// so different conversations can submit independently (test 8 of the Build 102
// marching orders), while FIFO remains strict inside each conversation. Only
// the matching attempt may transition or release its lease; a stale callback
// from an older task must not clear a lease held by a newer one.
//
// `acquiredAt` is used as the audit timestamp in structured logs and is the
// tie-breaker when an attempt reuses an attemptID after process restart.
struct SubmitLease: Equatable, Codable, Sendable {
    let conversationID: UUID
    let clientMessageID: UUID
    let attemptID: UUID
    let acquiredAt: Date
}

// Typed terminal result of a streaming attempt. The first of these to fire
// wins the consumer/watchdog race in `runStreamingAttempt`; the other branch
// is cancelled via structured concurrency. Every code path that ends a stream
// MUST be representable here — including the ones the legacy polling watchdog
// conflated under `stallDetected = true`.
//
// `.stalled(jobID)` carries the relay-assigned job so the polling fallback
// can reattach to the same job. Without that ID, recovery would mint a second
// Hermes run for the same clientMessageID — which is exactly the "no reply"
// bug Build 101 left on the floor.
enum StreamTerminal: Sendable, Equatable {
    case completed
    case retryableTransportFailure(guidance: String, retryAfterSeconds: Double?)
    case permanentFailure(category: String, message: String)
    case stalled(jobID: UUID)
    case cancelledByUser

    var isRecoverable: Bool {
        switch self {
        case .completed, .cancelledByUser: return false
        case .retryableTransportFailure, .stalled: return true
        case .permanentFailure: return false
        }
    }
}
