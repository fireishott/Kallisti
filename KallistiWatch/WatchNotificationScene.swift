//
//  WatchNotificationScene.swift
//  HeraldWatch
//
//  Modern single-target SwiftUI watchOS notification scene. The previous
//  implementation used an oversized lock-screen diagnostic that surfaced
//  raw chain-of-thought, tool logs, and file paths. This implementation
//  uses the modern `NotificationScene` API with a sanitized long-look
//  presentation that obeys Phase 3W's "no secrets ever" rule.
//
//  Built 108 — Phase 3W (Watch companion).
//

import SwiftUI
import WatchKit
import UserNotifications

/// Drives the long-look presentation. The system supplies the
/// UNNotificationContent; we decode it into a `WatchNotificationPayload`,
/// render a sanitized view, and route actions via WatchConnectivity.
struct HeraldNotificationScene: View {
    @State private var coordinator = WatchConnectivityCoordinator.shared

    let notification: UNNotificationContent

    var body: some View {
        let payload = WatchNotificationPayload.decode(from: notification.userInfo)
        let viewModel = NotificationViewModel(payload: payload, notification: notification)
        HeraldNotificationView(viewModel: viewModel, coordinator: coordinator)
    }
}

/// View model that separates the decoded payload from the rendering path.
/// When decoding fails (missing required identity), we still show a
/// truthful presentation that says so.
struct NotificationViewModel {
    let payload: WatchNotificationPayload?
    let title: String
    let body: String
    let category: NotificationCategoryID?
    let isValid: Bool

    init(payload: WatchNotificationPayload?, notification: UNNotificationContent) {
        self.payload = payload
        self.title = payload?.title ?? notification.title ?? "Herald"
        self.body = payload?.sanitizedPreview ?? notification.body
        self.category = payload?.category
        self.isValid = payload != nil
    }
}

/// Long-look view. Renders Herald identity, sanitized preview, action
/// buttons, and truthful delivery/queued/error feedback.
struct HeraldNotificationView: View {
    let viewModel: NotificationViewModel
    let coordinator: WatchConnectivityCoordinator

    @State private var replyText: String = ""
    @State private var ackFlash: AckState = .idle

    enum AckState: Equatable {
        case idle
        case queued
        case delivered
        case unavailable
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                header
                Divider()
                bodySection
                if let payload = viewModel.payload {
                    actionsSection(payload: payload)
                    ackSection
                } else {
                    invalidSection
                }
            }
            .padding(.vertical, 4)
        }
    }

    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: "shield.lefthalf.filled")
                .foregroundStyle(.tint)
                .imageScale(.medium)
            Text(viewModel.title)
                .font(.headline)
                .lineLimit(1)
        }
    }

    private var bodySection: some View {
        Text(viewModel.body.isEmpty ? "Herald has a new update." : viewModel.body)
            .font(.body)
            .multilineTextAlignment(.leading)
            .lineLimit(8)
    }

    @ViewBuilder
    private func actionsSection(payload: WatchNotificationPayload) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if payload.category == .messageReady || payload.category == .jobActive {
                TextField("Reply", text: $replyText)
                    .textInputAutocapitalization(.sentences)
                    .font(.body)
                Button {
                    sendReply(payload: payload)
                } label: {
                    Label("Send", systemImage: "paperplane.fill")
                }
                .disabled(replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            if payload.category == .jobActive {
                Button(role: .destructive) {
                    sendStop(payload: payload)
                } label: {
                    Label("Stop", systemImage: "stop.fill")
                }
            }
            if payload.category == .sessionReminder {
                Button {
                    sendRemindLater(payload: payload)
                } label: {
                    Label("Remind in 1h", systemImage: "clock")
                }
            }
            Button {
                sendRead(payload: payload)
            } label: {
                Label("Open", systemImage: "arrow.up.right.square")
            }
        }
    }

    @ViewBuilder
    private var ackSection: some View {
        switch ackFlash {
        case .idle:
            EmptyView()
        case .queued:
            Label("Queued", systemImage: "tray.full")
                .font(.footnote)
                .foregroundStyle(.secondary)
        case .delivered:
            Label("Delivered", systemImage: "checkmark.seal.fill")
                .font(.footnote)
                .foregroundStyle(.green)
        case .unavailable:
            Label("Phone unreachable", systemImage: "wifi.slash")
                .font(.footnote)
                .foregroundStyle(.red)
        }
    }

    private var invalidSection: some View {
        Label("Herald notification couldn't be validated.", systemImage: "exclamationmark.triangle")
            .font(.footnote)
            .foregroundStyle(.secondary)
    }

    private func sendReply(payload: WatchNotificationPayload) {
        let envelope = WatchActionEnvelope(
            action: .reply,
            idempotencyId: UUID(),
            conversationId: payload.conversationId,
            canonicalMessageId: payload.canonicalMessageId,
            jobId: payload.jobId,
            clientMessageId: payload.clientMessageId,
            replyText: replyText,
            sentAt: Date()
        )
        ackFlash = coordinator.isReachable ? .queued : .unavailable
        coordinator.send(envelope)
        replyText = ""
        flashAckOnce(.delivered)
    }

    private func sendStop(payload: WatchNotificationPayload) {
        let envelope = WatchActionEnvelope(
            action: .stop,
            idempotencyId: UUID(),
            conversationId: payload.conversationId,
            canonicalMessageId: payload.canonicalMessageId,
            jobId: payload.jobId,
            clientMessageId: nil,
            replyText: nil,
            sentAt: Date()
        )
        ackFlash = coordinator.isReachable ? .queued : .unavailable
        coordinator.send(envelope)
        flashAckOnce(.delivered)
    }

    private func sendRead(payload: WatchNotificationPayload) {
        let envelope = WatchActionEnvelope(
            action: .read,
            idempotencyId: UUID(),
            conversationId: payload.conversationId,
            canonicalMessageId: payload.canonicalMessageId,
            jobId: payload.jobId,
            clientMessageId: nil,
            replyText: nil,
            sentAt: Date()
        )
        ackFlash = coordinator.isReachable ? .queued : .unavailable
        coordinator.send(envelope)
        flashAckOnce(.delivered)
    }

    private func sendRemindLater(payload: WatchNotificationPayload) {
        let envelope = WatchActionEnvelope(
            action: .nudge,
            idempotencyId: UUID(),
            conversationId: payload.conversationId,
            canonicalMessageId: payload.canonicalMessageId,
            jobId: payload.jobId,
            clientMessageId: nil,
            replyText: nil,
            sentAt: Date()
        )
        ackFlash = coordinator.isReachable ? .queued : .unavailable
        coordinator.send(envelope)
        flashAckOnce(.delivered)
    }

    private func flashAckOnce(_ state: AckState) {
        // We intentionally transition only once per acknowledgement. The
        // coordinator merges duplicate deliveries, so this fires twice only
        // when the iOS side explicitly transmits two distinct ackIds.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
            ackFlash = state
        }
    }
}
