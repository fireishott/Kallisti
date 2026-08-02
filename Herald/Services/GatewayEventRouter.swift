import Foundation
import os

/// Routes gateway events to the appropriate session projection store.
///
/// This service bridges the `GatewayClient` event stream to the
/// `SessionProjectionStore`. It:
/// - Routes events by `session_id`
/// - Maps gateway event types to `TranscriptReducer` events
/// - Updates backend busy state
/// - Handles connection state changes
///
/// It does NOT:
/// - Make agent decisions
/// - Invent messages or tool calls
/// - Override backend authority
actor GatewayEventRouter {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "GatewayEventRouter")

    private let gatewayClient: GatewayClient
    private let projectionStore: SessionProjectionStore
    private var routingTask: Task<Void, Never>?

    init(gatewayClient: GatewayClient, projectionStore: SessionProjectionStore) {
        self.gatewayClient = gatewayClient
        self.projectionStore = projectionStore
    }

    /// Start routing events from the gateway client to the projection store.
    func startRouting() {
        routingTask = Task { [weak self] in
            guard let self else { return }
            for await event in await self.gatewayClient.events {
                await self.routeEvent(event)
            }
        }
    }

    /// Stop routing events.
    func stopRouting() {
        routingTask?.cancel()
        routingTask = nil
    }

    /// Route a single gateway event to the appropriate session.
    private func routeEvent(_ event: GatewayEvent) async {
        // Log unknown events for diagnostics
        guard let eventType = GatewayEventType(rawValue: event.type) else {
            Self.logger.info("Unknown gateway event type: \(event.type)")
            return
        }

        // Route by session ID if present
        if let sessionIDString = event.sessionID,
           let sessionID = CanonicalConversationID(sessionIDString) {
            await routeSessionEvent(event, sessionID: sessionID, eventType: eventType)
        } else {
            // Unscoped events
            await routeUnscopedEvent(event, eventType: eventType)
        }
    }

    /// Route a session-scoped event.
    private func routeSessionEvent(
        _ event: GatewayEvent,
        sessionID: CanonicalConversationID,
        eventType: GatewayEventType
    ) async {
        let epoch = await projectionStore.currentEpoch

        switch eventType {
        case .messageStart:
            // Assistant job binding
            if let jobIDString = event.payload?["job_id"]?.stringValue,
               let jobID = JobID(jobIDString) {
                let binding = TranscriptReducer.AssistantJobBinding(
                    conversationID: sessionID,
                    jobID: jobID,
                    canonicalMessageID: nil,
                    renderID: TranscriptRenderID(),
                    displayContent: "",
                    retryGeneration: nil
                )
                await projectionStore.applyAssistantJobBound(binding, epoch: epoch)
            }

        case .messageDelta:
            // Stream delta
            if let jobIDString = event.payload?["job_id"]?.stringValue,
               let jobID = JobID(jobIDString),
               let content = event.payload?["content"]?.stringValue {
                let delta = TranscriptReducer.TranscriptStreamDelta(
                    conversationID: sessionID,
                    jobID: jobID,
                    canonicalMessageID: nil,
                    conversationRevision: .zero,
                    messageRevision: .zero,
                    displayContent: content
                )
                await projectionStore.applyStreamDelta(delta, epoch: epoch)
            }

        case .messageComplete:
            // Terminal event (success)
            if let jobIDString = event.payload?["job_id"]?.stringValue,
               let jobID = JobID(jobIDString) {
                let terminal = TranscriptReducer.TranscriptTerminalEvent(
                    conversationID: sessionID,
                    jobID: jobID,
                    canonicalMessageID: nil,
                    conversationRevision: .zero,
                    messageRevision: .zero,
                    displayContent: event.payload?["content"]?.stringValue ?? "",
                    failure: nil
                )
                await projectionStore.applyMessageTerminal(terminal, epoch: epoch)
            }

        case .error:
            // Terminal event (failure)
            if let jobIDString = event.payload?["job_id"]?.stringValue,
               let jobID = JobID(jobIDString) {
                let failure = TranscriptFailure(
                    category: event.payload?["code"]?.stringValue ?? "unknown",
                    message: event.payload?["message"]?.stringValue ?? "Unknown error",
                    retryable: false
                )
                let terminal = TranscriptReducer.TranscriptTerminalEvent(
                    conversationID: sessionID,
                    jobID: jobID,
                    canonicalMessageID: nil,
                    conversationRevision: .zero,
                    messageRevision: .zero,
                    displayContent: "",
                    failure: failure
                )
                await projectionStore.applyMessageTerminal(terminal, epoch: epoch)
            }

        case .toolStart, .toolProgress, .toolComplete:
            // Tool events — update tool activity on the assistant row
            // For now, just log them
            Self.logger.info("Tool event: \(event.type) for session \(sessionID.rawValue)")

        case .reasoningDelta, .thinkingDelta:
            // Reasoning events — update reasoning on the assistant row
            Self.logger.info("Reasoning event: \(event.type) for session \(sessionID.rawValue)")

        default:
            Self.logger.info("Unhandled session event: \(event.type)")
        }
    }

    /// Route an unscoped event (no session_id).
    private func routeUnscopedEvent(_ event: GatewayEvent, eventType: GatewayEventType) async {
        switch eventType {
        case .gatewayReady:
            Self.logger.info("Gateway ready")
            await projectionStore.updateConnectionState(.connected)

        case .statusUpdate:
            // Update connection state based on status
            if let connected = event.payload?["connected"]?.boolValue {
                await projectionStore.updateConnectionState(connected ? .connected : .disconnected)
            }

        case .sessionsChanged:
            // Session list changed — could trigger a refresh
            Self.logger.info("Sessions changed")

        default:
            Self.logger.info("Unhandled unscoped event: \(event.type)")
        }
    }
}

// MARK: - GatewayValue extensions

extension GatewayValue {
    var stringValue: String? {
        if case .string(let v) = self { return v }
        return nil
    }

    var intValue: Int? {
        if case .int(let v) = self { return v }
        return nil
    }

    var boolValue: Bool? {
        if case .bool(let v) = self { return v }
        return nil
    }
}
