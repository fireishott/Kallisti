import SwiftUI

import Foundation

/// Real embedded terminal for TUI chat mode.
///
/// Connects to the connector's /v1/terminal WebSocket, which spawns
/// `hermes --tui` in a real PTY on the host. This view renders the actual
/// Hermes CLI - ANSI colors, cursor movement, panels, prompt - via
/// SwiftTerm, and forwards keyboard input back through the socket.
///
/// Nothing here is hardcoded: the base URL comes from the app's existing
/// connector/session configuration and auth rides the app's current
/// native access token, both passed in at start().
struct NativeTerminalView: UIViewRepresentable {
    typealias UIViewType = KallistiTerminalHostView

    @ObservedObject var model: NativeTerminalModel

    func makeUIView(context: Context) -> KallistiTerminalHostView {
        let view = KallistiTerminalHostView(frame: .zero)
        view.terminalDelegate = context.coordinator
        context.coordinator.view = view
        model.terminalView = view
        view.terminal.backgroundColor = TerminalColor(
            red: UInt16(0.047 * 65535), green: UInt16(0.047 * 65535), blue: UInt16(0.063 * 65535)
        )
        model.setViewHandler { [weak view] bytes in
            DispatchQueue.main.async {
                view?.feed(byteArray: ArraySlice(bytes))
            }
        }
        // Build 128.93: install the explicit touch-scroll pan recognizer
        // before returning so the very first touch the user makes on the
        // terminal is handled by it. Idempotent across SwiftUI rebuilds
        // (the extension method guards on `scrollPanRecognizer` being nil),
        // so this is safe on every makeUIView invocation.
        view.attachTouchScrollPanIfNeeded()
        // Build 128.98: `touchScrollEnabled` defaults to false (select
        // mode), so start the pan recognizer in the disabled state. The
        // recognizer is still installed so updateUIView can flip it on
        // the moment the user toggles scroll mode - this keeps the
        // recognizer identity stable across rebuilds (no reattach).
        view.setScrollPanEnabled(model.touchScrollEnabled)
        return view
    }

    func updateUIView(_ uiView: KallistiTerminalHostView, context: Context) {
        // Build 128.98: keep the touch-scroll pan recognizer in sync
        // with the toolbar toggle. When `touchScrollEnabled` is false
        // (select mode) the pan recognizer is disabled so drags go
        // through the fork's selection / mouse-reporting codepath
        // instead of driving scrollDown()/scrollUp(). When true (scroll
        // mode) the pan is enabled and vertical drags row-scroll the
        // buffer regardless of the app's mouse request state.
        uiView.setScrollPanEnabled(model.touchScrollEnabled)
        // Build 128.88: SwiftUI representables do not reliably drive
        // TerminalView.layoutSubviews with their final size. Without this
        // push, a TerminalView created at frame .zero keeps its zero-sized
        // terminal buffer and renders nothing - the black screen seen on
        // 128.87. This mirrors the vendored SwiftUITerminalHostView pattern
        // (updateSizeIfNeeded() from both layoutSubviews and updateUIView).
        uiView.updateSizeIfNeeded()
        // Build 128.93 TUI scroll fix: the SwiftTerm fork only installs its
        // panMouseGesture when the PTY requests mouse reporting, and
        // hermes --tui keeps mouseMode off (it never asks for mouse
        // events). With panMouseGesture missing, and 128.91 wiring
        // allowMouseReporting=false routing panSelectionHandler into
        // sendKey(arrow) (iOSTerminalView.swift ~line 1086) instead of
        // scrolling, the toolbar toggle sent arrow keys to the PTY on
        // every swipe - matching Curtis's "I can highlight but not scroll
        // via touch" report. 128.93 routes touch scroll through an
        // explicit UIPanGestureRecognizer that lives on the host view
        // (installed once in makeUIView) so vertical drags always drive
        // scrollDown() / scrollUp() rows regardless of mouse-mode state.
        // The toolbar toggle stays as a UX affordance but no longer flips
        // allowMouseReporting - both modes now scroll identically.
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model)
    }

    final class Coordinator: NSObject, TerminalViewDelegate {
        let model: NativeTerminalModel
        weak var view: TerminalView?

        init(model: NativeTerminalModel) {
            self.model = model
        }

        func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {
            model.resize(cols: newCols, rows: newRows)
        }

        func send(source: TerminalView, data: ArraySlice<UInt8>) {
            model.send(Array(data))
            // Build 128.89: keyboard auto-dismiss on Enter removed at
            // Curtis's request - he wants the keyboard to stay up for
            // consecutive commands.
        }

        func send(source: TerminalView, key: String) {
            model.send(Array(key.utf8))
            // Build 128.89: keyboard auto-dismiss on Enter removed at
            // Curtis's request - keyboard stays up for consecutive commands.
        }

        func scrolled(source: TerminalView, position: Double) {}
        func setTerminalTitle(source: TerminalView, title: String) {}
        func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
        func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {}
        func bell(source: TerminalView) {}
        func clipboardCopy(source: TerminalView, content: Data) {}
        func clipboardRead(source: TerminalView) -> Data? { nil }
        func iTermContent(source: TerminalView, content: ArraySlice<UInt8>) {}
        func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}
    }
}

/// Shared model: owns the WebSocket, pumps bytes to the view, forwards
/// input + resize frames back to the bridge.
/// Build 128.93: how the PTY bridge should start this terminal session.
enum TerminalSessionMode: Equatable {
    case new
    case resume
}

final class NativeTerminalModel: ObservableObject {
    @Published var isConnected = false
    @Published var statusText = "connecting to hermes..."
    /// Build 128.91: touch mode toggle. Default false = touch select
    /// (allowMouseReporting true: taps forward to the app as mouse events
    /// when the app requests them). When true = touch scroll, the terminal
    /// forces the selection/panning codepath so swipes always scroll the
    /// buffer regardless of the app's mouse request.
    @Published var touchScrollEnabled = false

    private var socket: URLSessionWebSocketTask?
    private var session: URLSession?
    private var viewHandler: (([UInt8]) -> Void)?
    /// Build 128.90: weak reference to the mounted terminal view so the
    /// model can resign first responder (keyboard dismissal button).
    weak var terminalView: TerminalView?
    /// Build 128.92: retained connection params so an unexpected socket
    /// death can reconnect without the screen having to restart it.
    private var baseURL: URL?
    private var token: String?
    /// Build 128.93: retained session choice so auto-reconnect after an
    /// unexpected socket death resumes the same session the user picked.
    private var terminalSessionMode = TerminalSessionMode.new
    /// Build 128.93: session id for resume mode (nil for new).
    private var terminalSessionId: String?
    /// Build 128.92: set by disconnect() so a deliberate teardown is not
    /// fought by a scheduled reconnect (mirrors NativeKallistiClient).
    private var isDeliberatelyDisconnected = false
    private var cols = 80
    private var rows = 24
    /// Build 128.89 (first-mount black screen): output frames that arrive
    /// before the SwiftUI representable has mounted (viewHandler still nil).
    /// On a settings toggle the WS can connect and stream the entire initial
    /// TUI render before makeUIView runs, so without this buffer those bytes
    /// are dropped and the terminal sits at a prompt on an empty screen.
    private var pendingBytes: [UInt8] = []

    func setViewHandler(_ handler: @escaping ([UInt8]) -> Void) {
        viewHandler = handler
        // Flush anything that arrived while the view was mounting, then
        // clear so a reconnect can't replay stale bytes. The handler
        // already hops to the main queue (makeUIView wraps feed in
        // DispatchQueue.main.async), so call it directly - wrapping it in
        // another async block trips Swift 6 @Sendable capture rules.
        guard !pendingBytes.isEmpty else { return }
        let bytes = pendingBytes
        pendingBytes.removeAll(keepingCapacity: true)
        handler(bytes)
    }

    /// Start the connection. `baseURL` is the app's resolved connector URL
    /// and `token` the current native access token - passed in from the
    /// caller, never hardcoded. Build 128.93: `sessionMode` carries the
    /// user's Resume / New choice (with `sessionId` for resume); the
    /// session handshake frame MUST be the first frame sent (before
    /// resize) because the bridge treats the first received frame as the
    /// handshake and defaults to `new` otherwise.
    func start(
        baseURL: URL,
        token: String?,
        sessionMode: TerminalSessionMode = .new,
        sessionId: String? = nil
    ) {
        guard socket == nil else { return }
        // Build 128.92: retain params for auto-reconnect, and clear the
        // deliberate-disconnect flag so a fresh start() can reconnect.
        self.baseURL = baseURL
        self.token = token
        self.terminalSessionMode = sessionMode
        self.terminalSessionId = sessionId
        isDeliberatelyDisconnected = false
        var components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false)
        components?.scheme = baseURL.scheme == "https" ? "wss" : "ws"
        components?.path = "/v1/terminal"
        guard let url = components?.url else {
            statusText = "bad url"
            return
        }
        var request = URLRequest(url: url)
        // Cookie-auth sessions have no bearer - the gateway session cookie
        // rides the request automatically via the shared cookie jar.
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        let config = URLSessionConfiguration.default
        session = URLSession(configuration: config)
        guard let session else { return }
        let task = session.webSocketTask(with: request)
        socket = task
        task.resume()
        isConnected = true
        statusText = "hermes --tui"

        receiveLoop(task)
        sendSessionHandshakeIfNeeded(mode: sessionMode)
        sendResize(cols: cols, rows: rows)
    }

    /// Build 128.93: send the session handshake frame FIRST (before any
    /// resize/input) so the bridge spawns `hermes --tui --resume <id>`
    /// when the user picked resume, or plain `hermes --tui` for new.
    /// The bridge's `_await_session_handshake()` waits on the first text
    /// frame with a short timeout; a missing frame defaults to new, which
    /// is the correct fallback for older clients.
    private func sendSessionHandshakeIfNeeded(mode: TerminalSessionMode) {
        guard let socket else { return }
        let payload: [String: Any]
        switch mode {
        case .new:
            payload = ["type": "session", "mode": "new"]
        case .resume:
            payload = ["type": "session", "mode": "resume", "sessionId": terminalSessionId ?? ""]
        }
        guard let data = try? JSONSerialization.data(withJSONObject: payload),
              let text = String(data: data, encoding: .utf8)
        else { return }
        socket.send(.string(text)) { _ in }
    }

    private func receiveLoop(_ task: URLSessionWebSocketTask) {
        task.receive { [weak self, weak task] result in
            guard let self, let task else { return }
            switch result {
            case .success(let message):
                switch message {
                case .string(let text):
                    self.handleFrame(text)
                case .data(let data):
                    if let text = String(data: data, encoding: .utf8) {
                        self.handleFrame(text)
                    }
                @unknown default:
                    break
                }
                self.receiveLoop(task)
            case .failure:
                self.isConnected = false
                self.statusText = "terminal closed"
                // Build 128.92: the socket died on its own (proxy idle reap,
                // network change, server restart). Without this the terminal
                // stays dead until the screen is restarted. Only reconnect
                // if this wasn't a deliberate disconnect() (tab switch).
                // The dead task is cleared first so start()'s socket == nil
                // guard passes; the reconnect itself runs inline here rather
                // than through a delayed closure, because a DispatchQueue /
                // Task closure capturing self trips Swift 6's sending checks
                // (NativeTerminalModel is not @MainActor).
                self.socket = nil
                self.maybeReconnect()
            }
        }
    }

    /// Build 128.92: reconnect after an unexpected socket death, gated by a
    /// timestamp so a flapping socket backs off instead of spinning. Called
    /// from the (nonisolated) URLSession callback, so no actor hop needed.
    private var lastReconnectAttemptAt: Date?
    private func maybeReconnect() {
        guard !isDeliberatelyDisconnected else { return }
        guard let baseURL else { return }
        if let last = lastReconnectAttemptAt,
           Date().timeIntervalSince(last) < 2.0 {
            return
        }
        lastReconnectAttemptAt = Date()
        statusText = "reconnecting..."
        // Build 128.93: re-attach with the same session choice the user
        // picked, so an unexpected socket death resumes the same TUI
        // session instead of silently starting fresh.
        start(
            baseURL: baseURL,
            token: token,
            sessionMode: terminalSessionMode,
            sessionId: terminalSessionId
        )
    }

    private func handleFrame(_ text: String) {
        guard let data = text.data(using: .utf8),
              let frame = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return }
        guard let type = frame["type"] as? String else { return }
        switch type {
        case "output":
            if let dataStr = frame["data"] as? String {
                let bytes = Array(dataStr.utf8)
                if let viewHandler {
                    viewHandler(bytes)
                } else {
                    // View not mounted yet - buffer so the initial render
                    // isn't dropped (Build 128.89).
                    pendingBytes.append(contentsOf: bytes)
                }
            }
        case "exit":
            isConnected = false
            statusText = "process exited"
        case "error":
            if let message = frame["message"] as? String {
                statusText = message
            }
        default:
            break
        }
    }

    func send(_ bytes: [UInt8]) {
        guard let socket else { return }
        let dataStr = String(decoding: bytes, as: UTF8.self)
        let payload: [String: Any] = ["type": "input", "data": dataStr]
        guard let data = try? JSONSerialization.data(withJSONObject: payload),
              let text = String(data: data, encoding: .utf8)
        else { return }
        socket.send(.string(text)) { _ in }
    }

    func resize(cols: Int, rows: Int) {
        guard cols > 0, rows > 0 else { return }
        self.cols = cols
        self.rows = rows
        sendResize(cols: cols, rows: rows)
    }

    private func sendResize(cols: Int, rows: Int) {
        guard let socket else { return }
        let payload: [String: Any] = ["type": "resize", "cols": cols, "rows": rows]
        guard let data = try? JSONSerialization.data(withJSONObject: payload),
              let text = String(data: data, encoding: .utf8)
        else { return }
        socket.send(.string(text)) { _ in }
    }

    func disconnect() {
        // Build 128.92: mark deliberate so an in-flight reconnect doesn't
        // fight the teardown (tab switch / screen gone).
        isDeliberatelyDisconnected = true
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        isConnected = false
    }

    /// Build 128.90: resign first responder on the terminal view so the
    /// software keyboard drops (manual dismiss - the Enter auto-dismiss was
    /// removed at Curtis's request). Called from the toolbar button, which
    /// runs on MainActor; resignFirstResponder is MainActor-isolated so
    /// assumeIsolated is safe here (and avoids a @Sendable async hop).
    func dismissKeyboard() {
        if let terminalView {
            MainActor.assumeIsolated {
                terminalView.resignFirstResponder()
            }
        }
    }
}

/// TerminalView subclass that actively pushes size changes into the terminal
/// on every layout pass - the same pattern SwiftTerm's own SwiftUI wrapper
/// (SwiftUITerminalHostView) uses. Without it, a TerminalView created at
/// frame .zero never learns its real bounds and renders nothing (black screen).
final class KallistiTerminalHostView: TerminalView {
    private var lastAppliedSize: CGSize = .zero
    /// Build 128.93: explicit touch-scroll pan recognizer. Created in
    /// `attachTouchScrollPanIfNeeded()` and never recreated (single install
    /// per view lifetime) so repeated SwiftUI `updateUIView` passes do not
    /// pile up overlapping recognizers.
    fileprivate var scrollPanRecognizer: UIPanGestureRecognizer?
    fileprivate var scrollPanDelegate: ScrollPanDelegate?

    override func layoutSubviews() {
        super.layoutSubviews()
        updateSizeIfNeeded()
    }

    func updateSizeIfNeeded() {
        let newSize = bounds.size
        guard newSize.width.isFinite, newSize.width > 0,
              newSize.height.isFinite, newSize.height > 0 else {
            return
        }
        if newSize != lastAppliedSize {
            lastAppliedSize = newSize
            processSizeChange(newSize: newSize)
        }
    }
}


// MARK: - Build 128.93: explicit touch-scroll pan recognizer
//
// The SwiftTerm fork only installs its `panMouseGesture` when the PTY has
// requested mouse reporting, and only attaches `panSelectionGesture` while
// the user is already selecting text. With `hermes --tui` keeping mouseMode
// off, neither pan recognizer is installed - drags fall through to the
// UIScrollView's native pan, which on device races `updateScroller()` on
// every feed and is easily missed. Worse, 128.91's
// `allowMouseReporting = !touchScrollEnabled` wiring flipped that flag to
// false in scroll mode, and the fork's `panSelectionHandler` non-selection
// branch routes the flag into `sendKey(deltaRow:)` (iOSTerminalView.swift
// ~line 1086) - i.e. arrow keys to the PTY, not scrolls.
//
// 128.93 fixes both with an explicit UIPanGestureRecognizer attached to
// the host view once on mount. It runs simultaneously with everything
// else (delaysTouchesBegan=false, cancelsTouchesInView=false, delegate
// yields `true` for simultaneousWith), translates vertical finger drag
// into row-by-row scrollDown()/scrollUp() calls on the public SwiftTerm
// API (which already drive contentOffset + userScrolling + caret follow
// correctly). Long-press select continues to work because the fork's
// 0.7s long-press recognizer's stationary-press activation criterion is
// unchanged; the new pan only consumes fast vertical movement.

private final class ScrollPanDelegate: NSObject, UIGestureRecognizerDelegate {
    weak var host: KallistiTerminalHostView?
    /// Pixels of vertical drag per row of buffer. Tuned so a normal finger
    /// swipe drags advance a few rows over its first 200pt of motion
    /// without overshooting.
    private let pixelsPerRow: CGFloat = 14.0

    func gestureRecognizer(
        _ gr: UIGestureRecognizer,
        shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
    ) -> Bool {
        // Coexist with UIScrollView's native pan, the 0.7s long-press, and
        // tap/double/triple tap. Returning false would cancel taps the
        // moment the user moves a few pixels - which is exactly what we
        // are here to make work.
        return true
    }

    func gestureRecognizerShouldBegin(_ gr: UIGestureRecognizer) -> Bool {
        // Reject purely-horizontal movement and tiny vertical flicks so a
        // quick side-swipe (which a user might mean as a selection-pivot
        // gesture) does not trigger a zero-row scroll, and so a stray
        // thumb graze during typing does not silently jump the buffer.
        guard let pan = gr as? UIPanGestureRecognizer else { return false }
        let v = pan.velocity(in: pan.view)
        return abs(v.y) > abs(v.x) && abs(v.y) > 60
    }

    @objc func handlePan(_ gr: UIPanGestureRecognizer) {
        guard let host else { return }
        switch gr.state {
        case .changed:
            let t = gr.translation(in: host)
            // Use translation rather than velocity so the scroll lands
            // exactly where the finger has stopped, not wherever the OS
            // had projected it to. Reset between events so each event is
            // a delta from the last.
            let dy = t.y
            gr.setTranslation(.zero, in: host)
            guard abs(dy) >= pixelsPerRow else { return }
            let rows = Int(dy / pixelsPerRow)
            if rows > 0 {
                // Finger moved DOWN on the screen -> scroll the buffer UP
                // (reveal earlier lines).
                for _ in 0..<rows { host.scrollUp(lines: 1) }
            } else if rows < 0 {
                for _ in 0..<(-rows) { host.scrollDown(lines: 1) }
            }
        default:
            break
        }
    }
}

extension KallistiTerminalHostView {
    /// Attach (once) the explicit touch-scroll pan recognizer. Idempotent:
    /// safe to call from every `makeUIView` pass because
    /// `scrollPanRecognizer` guards the install. The recognizer is additive
    // - it does not detach the fork's long-press, taps, or the
    /// underlying UIScrollView's native pan; each retains its original
    /// ARB relationship with the others.
    /// Build 128.98: flip the installed touch-scroll pan recognizer
    /// on/off without re-attaching it. Called from
    /// `NativeTerminalView/updateUIView` whenever the
    /// `touchScrollEnabled` toggle flips, and from `makeUIView` to
    /// seed the recognizer to the model's default (false). Cheap -
    /// just sets `isEnabled` on the existing recognizer; SwiftUI's
    /// representable rebuilds do not stack duplicates because the
    /// recognizer is created once in `attachTouchScrollPanIfNeeded`.
    func setScrollPanEnabled(_ enabled: Bool) {
        scrollPanRecognizer?.isEnabled = enabled
    }

    func attachTouchScrollPanIfNeeded() {
        guard scrollPanRecognizer == nil else { return }
        let delegate = ScrollPanDelegate()
        delegate.host = self
        scrollPanDelegate = delegate
        let pan = UIPanGestureRecognizer(
            target: delegate,
            action: #selector(ScrollPanDelegate.handlePan(_:))
        )
        pan.minimumNumberOfTouches = 1
        pan.maximumNumberOfTouches = 1
        pan.delaysTouchesBegan = false
        pan.cancelsTouchesInView = false
        pan.delegate = delegate
        addGestureRecognizer(pan)
        scrollPanRecognizer = pan
    }
}

