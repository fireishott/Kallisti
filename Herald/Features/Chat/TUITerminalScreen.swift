import SwiftUI


/// Full-screen host for the real embedded terminal (TUI chat mode).
///
/// Resolves the connector base URL and native access token from the app's
/// existing stores - nothing hardcoded - then presents SwiftTerm live against
/// the /v1/terminal PTY bridge running `hermes --tui`.
///
/// Build 128.93: before opening the WebSocket the screen queries
/// ``/v1/terminal/sessions`` for resumable hermes sessions. If any are
/// returned the user sees a one-tap confirmationDialog (Resume "title"
/// / Start new session / Cancel). The chosen session id rides the WS
/// handshake to the bridge, which spawns ``hermes --tui --resume <id>``
/// instead of a fresh TUI. With no resumable sessions (first launch,
/// clean state) the dialog is skipped and the screen goes straight to
/// a new session, matching the old behaviour.
struct TUITerminalScreen: View {
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(HeraldCanvasStore.self) private var canvasStore
    @Environment(AppContainer.self) private var container

    @State private var model = NativeTerminalModel()
    @State private var started = false
    @State private var loadError: String?

    /// Build 128.93: resumable sessions fetched from the connector for
    /// the "resume vs new" prompt. Nil = not yet fetched, empty = nothing
    /// resumable so we skip the prompt and go straight to a fresh TUI.
    @State private var resumableSessions: [ResumableSession]?
    /// Build 128.93: shown to drive the confirmationDialog. Toggled
    /// after the fetch resolves so SwiftUI animates the sheet correctly
    /// even when the prompt has nothing to ask (in which case we
    /// immediately start the new session instead of presenting).
    @State private var showSessionPrompt = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            if let loadError {
                VStack(spacing: 12) {
                    Image(systemName: "terminal")
                        .font(.system(size: 40))
                        .foregroundStyle(Design.Colors.accent)
                    Text("terminal unavailable")
                        .font(Design.Typography.code)
                        .foregroundStyle(Design.Colors.foreground)
                    Text(loadError)
                        .font(Design.Typography.codeSmall)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 24)
                    Button("Retry") {
                        started = false
                        self.loadError = nil
                        self.resumableSessions = nil
                        Task { await startTerminal() }
                    }
                    .buttonStyle(.borderedProminent)
                }
            } else {
                NativeTerminalView(model: model)
                    // Do NOT ignore the bottom safe area: when the software
                    // keyboard appears the safe-area inset grows and the view
                    // auto-resizes to sit above it. Ignoring it would extend
                    // the terminal underneath the keyboard (rows covered,
                    // no resize). sizeChanged fires on the new frame and
                    // pushes rows/cols to the PTY so hermes reflows.
            }
        }
        // Build 128.93: resume-vs-new picker. Shown only when there is at
        // least one resumable session; the action calls back into
        // beginTerminal(mode:sessionId:) which is the same code path the
        // "no sessions found / skip prompt" branch takes.
        .confirmationDialog(
            "Resume a previous TUI session?",
            isPresented: $showSessionPrompt,
            titleVisibility: .visible
        ) {
            if let first = resumableSessions?.first {
                Button("Resume \"\(first.title)\"") {
                    Task { await beginTerminal(mode: .resume, sessionId: first.id) }
                }
            }
            Button("Start new session") {
                Task { await beginTerminal(mode: .new, sessionId: nil) }
            }
            Button("Cancel", role: .cancel) {
                // Tear down the dialog but leave the screen mountable so
                // the user can swipe back and retry - no WS opened.
                showSessionPrompt = false
                started = false
            }
        } message: {
            Text(resumableSessions?.first.map { "Last active session: \($0.title)" } ?? "")
        }
        .task {
            guard !started else { return }
            await startTerminal()
        }
        // Build 128.92: .task does NOT reliably re-fire when the Chat tab
        // is re-selected in a TabView - the tab content stays alive and
        // SwiftUI only re-runs .task for the top-level tab view, not this
        // nested screen. Server evidence: terminal WS closed on tab-away
        // (08:59:31), no new /v1/terminal connection after return even
        // though the app kept polling. onAppear fires on EVERY tab return,
        // so drive the restart from here. Idempotent: startTerminal()
        // guards on `started` (set true immediately, so concurrent .task
        // + onAppear can't double-connect) and model.start() guards on
        // socket == nil.
        .onAppear {
            guard !started else { return }
            Task { await startTerminal() }
        }
        .onDisappear {
            model.disconnect()
            // Build 128.90: allow the next .task to restart the terminal.
            // Without this, leaving TUI (e.g. Settings tab) disconnects the
            // socket and the guard above skips on return, leaving the screen
            // frozen on "terminal closed".
            started = false
        }
        .toolbar {
            ToolbarItem(placement: .principal) {
                Text(model.statusText)
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(
                        model.isConnected ? Design.Colors.accent : Design.Colors.secondaryForeground
                    )
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    model.send([27])  // ESC - interrupts the running screen
                } label: {
                    Image(systemName: "escape")
                }
            }
            // Build 128.91: touch mode toggle. Default is touch select
            // (taps forward to the app as mouse events when it requests
            // them). Flip to touch scroll so swipes always pan the buffer
            // even when the app wants mouse reporting.
            //
            // Build 128.98: visual state. When scroll mode is on, render
            // the icon inside a subtle capsule (accent-tinted fill + thin
            // stroke) so the active mode is unambiguous at a glance - a
            // plain foreground/background swap is easy to miss on the
            // dark toolbar. The off state uses a near-transparent
            // background so the button still reads as a button, just
            // inactive. Tapping the button still just toggles the
            // boolean; the visual is derived from it.
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    model.touchScrollEnabled.toggle()
                } label: {
                    Image(systemName: model.touchScrollEnabled ? "arrow.up.and.down" : "hand.tap")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(
                            model.touchScrollEnabled ? Design.Colors.accent : Design.Colors.foreground
                        )
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .background {
                            if model.touchScrollEnabled {
                                Capsule(style: .continuous)
                                    .fill(Design.Colors.accent.opacity(0.18))
                                    .overlay(
                                        Capsule(style: .continuous)
                                            .stroke(Design.Colors.accent.opacity(0.55), lineWidth: 1)
                                    )
                            } else {
                                Capsule(style: .continuous)
                                    .fill(Color.white.opacity(0.04))
                            }
                        }
                }
                .accessibilityLabel(model.touchScrollEnabled ? "Touch scroll: on" : "Touch select: on")
                .accessibilityValue(model.touchScrollEnabled ? "Enabled" : "Disabled")
            }
            // Build 128.90: manual keyboard dismissal. The Enter
            // auto-dismiss was removed at Curtis's request; this button
            // resigns first responder so the terminal gets full screen
            // after typing.
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    model.dismissKeyboard()
                } label: {
                    Image(systemName: "keyboard.chevron.compact.down")
                }
                .accessibilityLabel("Dismiss keyboard")
            }
        }
    }

    private func startTerminal() async {
        // Build 128.93: every entry point (cold launch, tab-return
        // wake) routes through here. `started` is set BEFORE the await
        // so .task + .onAppear racing on first mount can't both start
        // the WS. The actual WS open happens in beginTerminal(), which
        // is invoked either immediately (no resumable sessions) or
        // after the user picks one in the confirmationDialog.
        started = true
        // Resolve live values from the app's own config - no hardcoding.
        guard let base = settingsStore.settings.relayConfiguration.activeBaseURLString,
              let baseURL = URL(string: base)
        else {
            loadError = "no relay host configured"
            return
        }
        let token: String? = await {
            if let nativeClient = container.nativeGatewayClient,
               let nativeToken = await nativeClient.refreshAccessToken(),
               !nativeToken.isEmpty {
                return nativeToken
            }
            return await sessionStore.currentAccessToken()
        }()
        if token == nil || token!.isEmpty {
            let cookieAuth = await container.nativeGatewayClient?.usesCookieAuth() ?? false
            guard cookieAuth else {
                loadError = "not signed in"
                return
            }
        }

        // First-time fetch of resumable sessions. Cheap call (one
        // ``hermes sessions list --limit 5`` on the connector host); if
        // it fails we just default to a new session so the user is never
        // blocked by the prompt code path.
        let fetched = await fetchResumableSessions(
            baseURL: baseURL,
            token: token
        )
        self.resumableSessions = fetched
        if let first = fetched.first {
            // Surface the dialog with the most-recent resumable session
            // preselected. We only offer the top one for now - the
            // simple "resume last" choice Curtis asked for - keeping
            // the UX a one-tap decision.
            _ = first
            showSessionPrompt = true
        } else {
            // Nothing to resume. Skip the prompt entirely so the
            // behaviour matches pre-128.93 on a clean install.
            await beginTerminal(mode: .new, sessionId: nil)
        }
    }

    /// Actually open the WS. Called either immediately (no prompt) or
    /// from a confirmationDialog button (Resume / Start new).
    private func beginTerminal(mode: TerminalSessionMode, sessionId: String?) async {
        guard let base = settingsStore.settings.relayConfiguration.activeBaseURLString,
              let baseURL = URL(string: base)
        else {
            loadError = "no relay host configured"
            return
        }
        let token: String? = await {
            if let nativeClient = container.nativeGatewayClient,
               let nativeToken = await nativeClient.refreshAccessToken(),
               !nativeToken.isEmpty {
                return nativeToken
            }
            return await sessionStore.currentAccessToken()
        }()
        model.start(
            baseURL: baseURL,
            token: token,
            sessionMode: mode,
            sessionId: sessionId
        )
    }

    /// GET /v1/terminal/sessions on the connector. Returns the parsed
    /// list or an empty array on any failure (auth, network, missing
    /// hermes CLI). Never throws - the caller must always be able to
    /// proceed with a default-new terminal.
    private func fetchResumableSessions(
        baseURL: URL,
        token: String?
    ) async -> [ResumableSession] {
        guard var components = URLComponents(
            url: baseURL,
            resolvingAgainstBaseURL: false
        ) else { return [] }
        components.path = "/v1/terminal/sessions"
        components.queryItems = [URLQueryItem(name: "limit", value: "5")]
        guard let url = components.url else { return [] }

        var request = URLRequest(url: url)
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        request.setValue("application/json", forHTTPHeaderField: "Accept")

        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse,
                  (200...299).contains(http.statusCode)
            else { return [] }
            return Self.decodeResumable(data)
        } catch {
            return []
        }
    }

    private static func decodeResumable(_ data: Data) -> [ResumableSession] {
        guard let obj = try? JSONSerialization.jsonObject(with: data),
              let dict = obj as? [String: Any],
              let arr = dict["sessions"] as? [[String: Any]]
        else { return [] }
        return arr.compactMap { row in
            guard let id = row["id"] as? String, !id.isEmpty else { return nil }
            let title = (row["title"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            return ResumableSession(
                id: id,
                title: title?.isEmpty == false ? title! : "Untitled session"
            )
        }
    }
}

/// Build 128.93: a session listed by ``/v1/terminal/sessions`` that
/// ``hermes --tui --resume`` can reattach to.
struct ResumableSession: Identifiable, Equatable {
    let id: String
    let title: String
}