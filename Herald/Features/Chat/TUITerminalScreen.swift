import SwiftUI


/// Full-screen host for the real embedded terminal (TUI chat mode).
///
/// Resolves the connector base URL and native access token from the app's
/// existing stores - nothing hardcoded - then presents SwiftTerm live against
/// the /v1/terminal PTY bridge running `hermes --tui`.
struct TUITerminalScreen: View {
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(HeraldCanvasStore.self) private var canvasStore
    @Environment(AppContainer.self) private var container

    @State private var model = NativeTerminalModel()
    @State private var started = false
    @State private var loadError: String?

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
        .task {
            guard !started else { return }
            await startTerminal()
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
        started = true
        // Resolve live values from the app's own config - no hardcoding.
        guard let base = settingsStore.settings.relayConfiguration.activeBaseURLString,
              let baseURL = URL(string: base)
        else {
            loadError = "no relay host configured"
            return
        }
        // Native mode never writes SecureKeys.accessToken (the legacy pairing
        // key) - the live credential is the native gateway bearer. Mirror
        // every other service: native token first, legacy fallback for
        // basic/pairing auth modes.
        // Resolve a bearer if one exists (native gateway bearer first,
        // legacy pairing token fallback). Basic/pairing login sessions have
        // NO bearer - login deletes the keychain token by design - so a nil
        // token here is normal in cookie-auth mode. The connector's
        // /v1/terminal accepts the gateway session cookie, and URLSession
        // attaches it automatically for the relay domain, so we must NOT
        // block connection on token presence when cookies are viable.
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
        model.start(baseURL: baseURL, token: token)
    }
}