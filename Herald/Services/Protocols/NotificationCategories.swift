//
//  NotificationCategories.swift
//  Herald
//
//  Shared definition of notification category and action identifiers used by
//  the main iOS app, the notification service extension, and the watchOS
//  companion. This is the single source of truth so string constants cannot
//  drift between targets.
//
//  Built 108 — Phase 3W (Watch companion).
//
//  IMPORTANT: enums are declared `internal` (the default) so the iOS host
//  target, the HeraldNotificationService extension, and the HeraldWatch
//  target can all see the same Swift symbols without visibility gymnastics.
//  Each target gets a header copy because the shared source file is added
//  to each target's `sources` list in `project.yml`.
//

import Foundation

/// Stable category identifiers. Mirrored by the NotificationService extension
/// (`HeraldNotificationService`) and the watchOS companion target.
enum NotificationCategoryID: String, CaseIterable, Sendable {
    /// Completed assistant reply — main, open-and-read path.
    case messageReady = "HERALD_MESSAGE_READY"
    /// A job is still running on the connector.
    case jobActive = "HERALD_JOB_ACTIVE"
    /// Time-sensitive session reminder.
    case sessionReminder = "HERALD_SESSION_REMINDER"

    var displayName: String {
        switch self {
        case .messageReady: return "Kallisti Reply"
        case .jobActive: return "Kallisti Job"
        case .sessionReminder: return "Kallisti Reminder"
        }
    }
}

/// Stable action identifiers. The full set is shared so that any target that
/// inspects a notification payload decodes the same string.
enum NotificationActionID: String, CaseIterable, Sendable {
    case read = "HERALD_ACTION_READ"
    case reply = "HERALD_ACTION_REPLY"
    case stop = "HERALD_ACTION_STOP"
    case nudge = "HERALD_ACTION_NUDGE"
    case remindLater = "HERALD_ACTION_REMIND_LATER"
    case dismiss = "HERALD_ACTION_DISMISS"

    /// True when this action is destructive (e.g. cancel a job).
    var isDestructive: Bool {
        switch self {
        case .stop, .dismiss: return true
        case .read, .reply, .nudge, .remindLater: return false
        }
    }

    /// True when the action must run on the main iOS app process (i.e. the
    /// companion Watch hands it off through WatchConnectivity).
    var requiresPhoneHandoff: Bool {
        switch self {
        case .reply, .stop, .read, .nudge: return true
        case .remindLater, .dismiss: return false
        }
    }
}

/// Wire field names for the notification payload userInfo dictionary. Phase 3A
/// v2 sets `conversationId` as the canonical conversation identifier; we do
/// not emit both `conversationId` and `canonicalConversationId`.
enum NotificationPayloadKey: String, Sendable {
    case conversationId = "conversationId"
    case canonicalMessageId = "canonicalMessageId"
    case jobId = "jobId"
    case clientMessageId = "clientMessageId"
    case attempt = "attempt"
    case category = "category"
    case contractVersion = "contractVersion"
    case sanitizedPreview = "sanitizedPreview"
    case title = "title"
    case pushType = "pushType"
    case idempotencyId = "idempotencyId"
    case fullResponse = "full_response"
}

/// The wire contract version this app emits. Phase 3A v2 uses v3 envelopes on
/// the SSE path; the notification payload carries a parallel `contractVersion`
/// so iOS, the NotificationService, and the Watch can all reject mismatched
/// payloads.
enum NotificationContractVersion {
    static let current: Int = 3
}
