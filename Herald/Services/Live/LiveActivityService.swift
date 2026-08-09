import ActivityKit
import Foundation

/// Privacy-safe lock-screen phases.  Never carry raw tool names, commands,
/// paths, prompts, or model output — only the generic categories below.
/// These are the ONLY values allowed in KallistiActivityAttributes.ContentState.
enum LiveActivityPhase: String, Sendable {
    case thinking = "Thinking\u{2026}"           // model loading / reasoning
    case usingTools = "Using tools\u{2026}"      // tool execution in progress
    case responding = "Responding\u{2026}"       // streaming text reply
    case waitingForHost = "Waiting for host\u{2026}"
    case needsAttention = "Needs attention"      // error / cancellation
    case done = "Done"

    /// Safe secondary detail line — nil for most phases.
    var detail: String? {
        switch self {
        case .thinking:     return nil
        case .usingTools:   return nil
        case .responding:   return nil
        case .waitingForHost: return nil
        case .needsAttention: return "Tap to open Herald"
        case .done:         return nil
        }
    }
}

/// Manages Herald Live Activities on the Lock Screen and Dynamic Island.
@MainActor
@Observable
final class LiveActivityService {
    private var currentActivity: Activity<KallistiActivityAttributes>?
    private var startedAt: Date?
    /// Current lock-screen phase — only these values enter ActivityKit state.
    private var lockScreenPhase: LiveActivityPhase = .thinking

    var isAvailable: Bool {
        ActivityAuthorizationInfo().areActivitiesEnabled
    }

    // MARK: - Voice Session

    func startVoiceSession() {
        guard isAvailable else { return }
        lockScreenPhase = .thinking  // voice starts in thinking/model-loading
        let now = Date.now
        adoptExistingActivityIfNeeded()
        let attributes = KallistiActivityAttributes(agentName: "Kallisti")
        let state = makeContentState(
            phase: .thinking, elapsedSeconds: 0, startDate: now, sessionType: "voice"
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
        lockScreenPhase = sanitizedPhase(for: status)
        let elapsed = Int(Date().timeIntervalSince(startedAt ?? .now))
        let state = makeContentState(
            phase: lockScreenPhase, elapsedSeconds: elapsed, startDate: startedAt, sessionType: "voice"
        )
        updateActivity(with: state)
    }

    // MARK: - Chat Streaming

    func startThinking() {
        guard isAvailable else { return }
        lockScreenPhase = .thinking
        let now = Date.now
        adoptExistingActivityIfNeeded()
        let attributes = KallistiActivityAttributes(agentName: "Kallisti")
        let state = makeContentState(
            phase: .thinking, elapsedSeconds: 0, startDate: now, sessionType: "chat"
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
        lockScreenPhase = sanitizedPhase(for: status)
        let elapsed = Int(Date().timeIntervalSince(startedAt ?? .now))
        let state = makeContentState(
            phase: lockScreenPhase, elapsedSeconds: elapsed, startDate: startedAt, sessionType: "chat"
        )
        updateActivity(with: state)
    }

    // MARK: - Chat / Tool Calls

    /// Build 32 (fix): tool names are NEVER passed to ActivityKit.
    /// Raw tool identifiers ("hermes_delegate", "Check for API keys...") are
    /// lock-screen unsafe.  The live activity shows only the generic
    /// "Using tools…" phase — details stay inside the unlocked app.
    func startToolCall(toolName: String) {
        guard isAvailable else { return }
        lockScreenPhase = .usingTools
        let now = Date.now
        adoptExistingActivityIfNeeded()
        let attributes = KallistiActivityAttributes(agentName: "Kallisti")
        let state = makeContentState(
            phase: .usingTools, elapsedSeconds: 0, startDate: now, sessionType: "tool"
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
        lockScreenPhase = .usingTools
        let elapsed = Int(Date().timeIntervalSince(startedAt ?? .now))
        let state = makeContentState(
            phase: .usingTools, elapsedSeconds: elapsed, startDate: startedAt, sessionType: "tool"
        )
        updateActivity(with: state)
    }

    // MARK: - End

    func endActivity() {
        startedAt = nil
        lockScreenPhase = .done
        guard currentActivity != nil else { return }
        currentActivity = nil

        let finalContent = ActivityContent(
            state: KallistiActivityAttributes.ContentState(
                status: LiveActivityPhase.done.rawValue,
                toolName: nil,
                elapsedSeconds: 0,
                startDate: nil,
                sessionType: "chat",
                emoji: "\u{2705}"
            ),
            staleDate: nil
        )
        Task.detached {
            for activity in Activity<KallistiActivityAttributes>.activities {
                await activity.end(finalContent, dismissalPolicy: .immediate)
            }
        }
    }

    // MARK: - Push Token Registration

    /// Notification posted when a new Live Activity push token is available.
    /// AppContainer observes this to register the token with the relay.
    static let pushTokenDidUpdateNotification = Notification.Name("KallistiLiveActivityPushTokenDidUpdate")

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
        if let defaults = UserDefaults(suiteName: "group.net.fihonline.kallisti") {
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

    private func updateActivity(with state: KallistiActivityAttributes.ContentState) {
        guard let activity = currentActivity, activity.activityState == .active else { return }
        let content = ActivityContent(state: state, staleDate: nil)
        let activityID = activity.id
        Task.detached {
            for activity in Activity<KallistiActivityAttributes>.activities where activity.id == activityID {
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
            state: KallistiActivityAttributes.ContentState(
                status: LiveActivityPhase.done.rawValue,
                toolName: nil,
                elapsedSeconds: 0,
                startDate: nil,
                sessionType: "chat",
                emoji: "\u{2705}"
            ),
            staleDate: nil
        )
        Task.detached {
            for activity in Activity<KallistiActivityAttributes>.activities {
                await activity.end(finalContent, dismissalPolicy: .default)
            }
        }
    }

    /// Build a privacy-safe ContentState.  Only the sanitized phase label and
    /// safe metadata enter ActivityKit state — raw tool names, commands, paths,
    /// and model output are NEVER exposed on the lock screen.
    private func makeContentState(
        phase: LiveActivityPhase,
        elapsedSeconds: Int = 0,
        startDate: Date? = nil,
        sessionType: String = "chat"
    ) -> KallistiActivityAttributes.ContentState {
        return KallistiActivityAttributes.ContentState(
            status: phase.rawValue,
            toolName: phase.detail,
            elapsedSeconds: elapsedSeconds,
            startDate: startDate,
            sessionType: sessionType,
            emoji: emojiFor(phase),
            gatewayConnected: false,
            activeQueries: 0,
            modelName: nil,
            version: nil,
            cpuPercent: 0.0,
            memoryUsedGb: 0.0,
            memoryTotalGb: 0.0,
            uptimeHours: 0.0,
            alertCount: 0
        )
    }

    /// Map an arbitrary internal status string to a privacy-safe lock-screen phase.
    /// Never passes raw tool names, commands, or model output to ActivityKit.
    private func sanitizedPhase(for status: String) -> LiveActivityPhase {
        let lower = status.lowercased()
        if lower.contains("think") || lower.contains("reason") || lower.contains("loading") {
            return .thinking
        }
        if lower.contains("tool") || lower.contains("execut") || lower.contains("working")
            || lower.contains("check") || lower.contains("search") || lower.contains("call") {
            return .usingTools
        }
        if lower.contains("respond") || lower.contains("stream") || lower.contains("answer")
            || lower.contains("generat") {
            return .responding
        }
        if lower.contains("wait") || lower.contains("reconnect") || lower.contains("stall") {
            return .waitingForHost
        }
        if lower.contains("fail") || lower.contains("error") || lower.contains("cancel")
            || lower.contains("interrupt") {
            return .needsAttention
        }
        // Default: responding (most common positive phase)
        return .responding
    }

    private func emojiFor(_ phase: LiveActivityPhase) -> String {
        switch phase {
        case .thinking:       return "\u{1F9E0}"  // 🧠 brain
        case .usingTools:     return "\u{26A1}"    // ⚡ lightning
        case .responding:     return "\u{1F4AC}"   // 💬 speech bubble
        case .waitingForHost: return "\u{23F3}"    // ⏳ hourglass
        case .needsAttention: return "\u{26A0}\u{FE0F}"  // ⚠️ warning
        case .done:           return "\u{2705}"    // ✅ checkmark
        }
    }

    private func adoptExistingActivityIfNeeded() {
        guard currentActivity == nil else { return }
        if let activity = Activity<KallistiActivityAttributes>.activities.first(where: { $0.activityState == .active }) {
            currentActivity = activity
            startedAt = activity.content.state.startDate
        }
    }
}
