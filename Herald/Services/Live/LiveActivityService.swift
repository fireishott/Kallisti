import ActivityKit
import Foundation

/// Manages Herald Live Activities on the Lock Screen and Dynamic Island.
@MainActor
@Observable
final class LiveActivityService {
    private var currentActivity: Activity<HeraldActivityAttributes>?
    private var startedAt: Date?

    var isAvailable: Bool {
        ActivityAuthorizationInfo().areActivitiesEnabled
    }

    // MARK: - Voice Session

    func startVoiceSession() {
        guard isAvailable else { return }
        let now = Date.now
        adoptExistingActivityIfNeeded()
        let attributes = HeraldActivityAttributes(agentName: "Herald")
        let state = makeContentState(
            status: "Listening", toolName: nil, elapsedSeconds: 0, startDate: now, sessionType: "voice",
            emoji: emojiForPhase("listening", sessionType: "voice")
        )
        if currentActivity != nil {
            startedAt = now
            updateActivity(with: state)
            return
        }
        do {
            currentActivity = try Activity.request(
                attributes: attributes,
                content: .init(state: state, staleDate: nil),
                pushType: .token
            )
            startedAt = now
            observePushTokens()
        } catch {
            // Live Activities not supported or disabled — silently ignore
        }
    }

    func updateVoiceState(_ status: String, toolName: String? = nil) {
        let elapsed = Int(Date().timeIntervalSince(startedAt ?? .now))
        let state = makeContentState(
            status: status, toolName: toolName, elapsedSeconds: elapsed, startDate: startedAt, sessionType: "voice",
            emoji: emojiForPhase(status, sessionType: "voice")
        )
        updateActivity(with: state)
    }

    // MARK: - Chat Streaming

    func startThinking() {
        guard isAvailable else { return }
        let now = Date.now
        adoptExistingActivityIfNeeded()
        let attributes = HeraldActivityAttributes(agentName: "Herald")
        let state = makeContentState(
            status: "Thinking", startDate: now, sessionType: "chat",
            emoji: emojiForPhase("thinking", sessionType: "chat")
        )
        if currentActivity != nil {
            startedAt = now
            updateActivity(with: state)
            return
        }
        do {
            currentActivity = try Activity.request(
                attributes: attributes,
                content: .init(state: state, staleDate: nil),
                pushType: .token
            )
            startedAt = now
            observePushTokens()
        } catch {
            // Live Activities not supported or disabled — silently ignore
        }
    }

    func updatePhase(_ status: String) {
        guard currentActivity != nil else { return }
        let elapsed = Int(Date().timeIntervalSince(startedAt ?? .now))
        let state = makeContentState(
            status: status, toolName: nil, elapsedSeconds: elapsed, startDate: startedAt, sessionType: "chat",
            emoji: emojiForPhase(status, sessionType: "chat")
        )
        updateActivity(with: state)
    }

    // MARK: - Chat / Tool Calls

    func startToolCall(toolName: String) {
        guard isAvailable else { return }
        let now = Date.now
        adoptExistingActivityIfNeeded()
        let attributes = HeraldActivityAttributes(agentName: "Herald")
        let state = makeContentState(
            status: "Working...", toolName: toolName, elapsedSeconds: 0, startDate: now, sessionType: "tool",
            emoji: emojiForPhase("working", sessionType: "tool")
        )
        if currentActivity != nil {
            startedAt = now
            updateActivity(with: state)
            return
        }
        do {
            currentActivity = try Activity.request(
                attributes: attributes,
                content: .init(state: state, staleDate: nil),
                pushType: .token
            )
            startedAt = now
            observePushTokens()
        } catch {
            // Silently ignore
        }
    }

    func updateToolProgress(_ status: String, toolName: String? = nil) {
        let elapsed = Int(Date().timeIntervalSince(startedAt ?? .now))
        let state = makeContentState(
            status: status, toolName: toolName, elapsedSeconds: elapsed, startDate: startedAt, sessionType: "tool",
            emoji: emojiForPhase(status, sessionType: "tool")
        )
        updateActivity(with: state)
    }

    // MARK: - End

    func endActivity() {
        startedAt = nil
        currentActivity = nil

        let finalContent = ActivityContent(
            state: HeraldActivityAttributes.ContentState(
                status: "Done", toolName: nil, elapsedSeconds: 0, startDate: nil, sessionType: "voice",
                emoji: nil
            ),
            staleDate: nil
        )
        Task.detached {
            for activity in Activity<HeraldActivityAttributes>.activities {
                await activity.end(finalContent, dismissalPolicy: .immediate)
            }
        }
    }

    // MARK: - Push Token Registration

    /// Notification posted when a new Live Activity push token is available.
    /// AppContainer observes this to register the token with the relay.
    static let pushTokenDidUpdateNotification = Notification.Name("HeraldLiveActivityPushTokenDidUpdate")

    /// Observe push token updates from the current activity and deliver them
    /// to the relay for remote activity updates.
    func observePushTokens() {
        guard let activity = currentActivity else { return }
        let activityRef = activity
        Task {
            for await token in activityRef.pushTokenUpdates {
                let tokenHex = token.map { String(format: "%02x", $0) }.joined()
                Self.registerLiveActivityPushToken(tokenHex)
            }
        }
    }

    /// Store the push token and notify AppContainer to register it with the relay.
    private static func registerLiveActivityPushToken(_ token: String) {
        // Store for cross-process access (widget extension can also read this)
        if let defaults = UserDefaults(suiteName: "group.net.fihonline.herald") {
            defaults.set(token, forKey: "herald.liveActivity.pushToken")
        }
        // Notify AppContainer to send the token to the relay
        Task { @MainActor in
            NotificationCenter.default.post(
                name: pushTokenDidUpdateNotification,
                object: token
            )
        }
    }

    // MARK: - Private

    private func updateActivity(with state: HeraldActivityAttributes.ContentState) {
        guard let activity = currentActivity, activity.activityState == .active else { return }
        let content = ActivityContent(state: state, staleDate: nil)
        let activityID = activity.id
        Task.detached {
            for activity in Activity<HeraldActivityAttributes>.activities where activity.id == activityID {
                await activity.update(content)
            }
        }
    }

    // MARK: - App Lifecycle

    /// Called when the app returns to foreground. No timer to restart —
    /// the widget uses Text(timerInterval:) which ticks natively via the OS.
    func handleAppDidBecomeActive() {
        adoptExistingActivityIfNeeded()
    }

    static func endAllActivities() {
        let finalContent = ActivityContent(
            state: HeraldActivityAttributes.ContentState(
                status: "Done", toolName: nil, elapsedSeconds: 0, startDate: nil, sessionType: "voice",
                emoji: nil
            ),
            staleDate: nil
        )
        Task.detached {
            for activity in Activity<HeraldActivityAttributes>.activities {
                await activity.end(finalContent, dismissalPolicy: .immediate)
            }
        }
    }

    /// Build a ContentState enriched with current gateway telemetry from the
    /// shared App Group store. All parameters except status have sensible defaults.
    private func makeContentState(
        status: String,
        toolName: String? = nil,
        elapsedSeconds: Int = 0,
        startDate: Date? = nil,
        sessionType: String = "chat",
        emoji: String? = nil
    ) -> HeraldActivityAttributes.ContentState {
        let gw = GatewayState.shared
        return HeraldActivityAttributes.ContentState(
            status: status,
            toolName: toolName,
            elapsedSeconds: elapsedSeconds,
            startDate: startDate,
            sessionType: sessionType,
            emoji: emoji,
            gatewayConnected: gw.isConnected,
            activeQueries: gw.activeJobs,
            modelName: gw.model,
            version: gw.version,
            cpuPercent: gw.cpuPercent,
            memoryUsedGb: gw.memoryUsedGb,
            memoryTotalGb: gw.memoryTotalGb,
            uptimeHours: Double(gw.uptimeSeconds) / 3600.0,
            alertCount: gw.alertCount
        )
    }

    private func adoptExistingActivityIfNeeded() {
        guard currentActivity == nil else { return }
        if let activity = Activity<HeraldActivityAttributes>.activities.first(where: { $0.activityState == .active }) {
            currentActivity = activity
            startedAt = activity.content.state.startDate
        }
    }

    private func emojiForPhase(_ phase: String, sessionType: String) -> String {
        switch phase.lowercased() {
        case "thinking", "reasoning": return "\u{1F9E0}"    // brain
        case "responding", "streaming": return "\u{1F4AC}"  // speech bubble
        case "working", "executing": return "\u{26A1}"      // lightning
        case "listening": return "\u{1F3A4}"                // microphone
        case "searching": return "\u{1F50D}"                // magnifying glass
        default:
            switch sessionType {
            case "voice": return "\u{1F3A4}"
            case "tool":  return "\u{1F527}"
            default:      return "\u{1F4AC}"
            }
        }
    }
}
