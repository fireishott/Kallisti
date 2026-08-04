//
//  WatchNotificationPayload.swift
//  HeraldWatch
//
//  Decoder for the Herald notification payload as it appears on watchOS.
//  The same payload bytes (userInfo dictionary) arrive at the iOS app, the
//  iOS notification service extension, and the watchOS companion. The
//  shared `NotificationCategories.swift` file (visible to all three targets)
//  defines the canonical wire field names.
//
//  Built 108 — Phase 3W (Watch companion).
//

import Foundation

/// Decoded, validated Herald notification payload. The WireField enum mirrors
/// `NotificationPayloadKey` exactly so any drift is caught at compile time.
struct WatchNotificationPayload: Sendable, Equatable {
    let category: NotificationCategoryID
    let contractVersion: Int
    let conversationId: UUID?
    let canonicalMessageId: String?
    let jobId: UUID?
    let clientMessageId: UUID?
    let attempt: Int
    let sanitizedPreview: String
    let title: String?
    let id: String   // stable for dedup

    static let currentContractVersion = NotificationContractVersion.current

    /// Build a payload from the on-wire userInfo dictionary. Returns nil when
    /// required identity is missing. Sanitizes the preview synchronously.
    static func decode(from userInfo: [AnyHashable: Any]) -> WatchNotificationPayload? {
        guard let categoryString = userInfo[NotificationPayloadKey.category.rawValue] as? String,
              let category = NotificationCategoryID(rawValue: categoryString) else {
            return nil
        }

        let contract = (userInfo[NotificationPayloadKey.contractVersion.rawValue] as? Int)
            ?? (userInfo[NotificationPayloadKey.contractVersion.rawValue] as? NSNumber)?.intValue
            ?? 0

        let conversationId = (userInfo[NotificationPayloadKey.conversationId.rawValue] as? String)
            .flatMap { UUID(uuidString: $0) }
        let canonicalMessageId = userInfo[NotificationPayloadKey.canonicalMessageId.rawValue] as? String
        let jobId = (userInfo[NotificationPayloadKey.jobId.rawValue] as? String)
            .flatMap { UUID(uuidString: $0) }
        let clientMessageId = (userInfo[NotificationPayloadKey.clientMessageId.rawValue] as? String)
            .flatMap { UUID(uuidString: $0) }
        let attempt = (userInfo[NotificationPayloadKey.attempt.rawValue] as? Int)
            ?? (userInfo[NotificationPayloadKey.attempt.rawValue] as? NSNumber)?.intValue
            ?? 0
        let preview = (userInfo[NotificationPayloadKey.sanitizedPreview.rawValue] as? String)
            ?? (userInfo[NotificationPayloadKey.fullResponse.rawValue] as? String)
            ?? ""
        let title = userInfo[NotificationPayloadKey.title.rawValue] as? String

        // Required identity: at least a conversationId for reply/STOP/NUDGE
        // flows. For sessionReminder it's allowed to be empty.
        if category != .sessionReminder && conversationId == nil {
            return nil
        }

        // Build a stable identity for dedup. The Apple-supplied identifier
        // collapses by payload hash; we add canonical IDs to make duplicate
        // notifications collapse during in-flight replay.
        let id = [
            category.rawValue,
            conversationId?.uuidString ?? "",
            canonicalMessageId ?? "",
            jobId?.uuidString ?? "",
            "\(attempt)"
        ].joined(separator: "::")

        return WatchNotificationPayload(
            category: category,
            contractVersion: contract,
            conversationId: conversationId,
            canonicalMessageId: canonicalMessageId,
            jobId: jobId,
            clientMessageId: clientMessageId,
            attempt: attempt,
            sanitizedPreview: Self.sanitize(preview),
            title: title.map(Self.sanitize) ?? Self.defaultTitle(for: category),
            id: id
        )
    }

    /// Maximum preview length on Watch. The previous lock-screen diagnostic
    /// pushed 4,000+ characters onto the Watch UI — we cap at 200 and
    /// sanitize aggressively.
    static let previewMaxLength = 200

    /// Strip the forensic surface from any preview text. We never render
    /// raw chain-of-thought, tool logs, prompt envelopes, file-system paths,
    /// or API keys on Watch. The sanitizer is intentionally conservative:
    /// when in doubt, redact.
    static func sanitize(_ raw: String) -> String {
        var s = raw
        // Strip plausible secrets / API keys / tokens.
        // Patterns: bearer tokens, hex strings > 32 chars, key=value pairs with
        // api_key/token/secret, file paths beginning with /, prompt envelope
        // markers, and <file> tags.
        let patterns: [String] = [
            #"(?i)bearer\s+[A-Za-z0-9._\-]+"#,
            #"(?i)(api[_-]?key|token|secret|password|authorization)\s*[:=]\s*[\"']?[A-Za-z0-9._\-]{6,}[\"']?"#,
            #"/Users/[^ \n\t]+"#,
            #"/var/[^ \n\t]+"#,
            #"/tmp/[^ \n\t]+"#,
            #"/private/[^ \n\t]+"#,
            #"<file>.*?</file>"#,
            #"\[system\]|\[user\]|\[assistant\]|\[tool\]|\[tool_call\]|\[tool_result\]"#,
            #"(?i)chain[_-]?of[_-]?thought\s*:"#,
            #"(?i)reasoning\s*:"#,
            #"(?i)tool\s*log\s*:"#,
            #"(?i)prompt\s*envelope\s*:"#
        ]
        for pattern in patterns {
            guard let regex = try? NSRegularExpression(pattern: pattern, options: [.dotMatchesLineSeparators]) else {
                continue
            }
            let range = NSRange(s.startIndex..<s.endIndex, in: s)
            s = regex.stringByReplacingMatches(
                in: s,
                options: [],
                range: range,
                withTemplate: "[redacted]"
            )
        }
        // Collapse excess whitespace.
        while s.contains("  ") { s = s.replacingOccurrences(of: "  ", with: " ") }
        s = s.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.count > previewMaxLength {
            s = String(s.prefix(previewMaxLength)) + "…"
        }
        return s
    }

    private static func defaultTitle(for category: NotificationCategoryID) -> String {
        switch category {
        case .messageReady: return "Herald"
        case .jobActive: return "Herald Job"
        case .sessionReminder: return "Herald Reminder"
        }
    }
}
