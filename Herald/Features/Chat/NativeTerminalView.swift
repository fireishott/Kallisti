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
        return view
    }

    func updateUIView(_ uiView: KallistiTerminalHostView, context: Context) {
        // Build 128.88: SwiftUI representables do not reliably drive
        // TerminalView.layoutSubviews with their final size. Without this
        // push, a TerminalView created at frame .zero keeps its zero-sized
        // terminal buffer and renders nothing - the black screen seen on
        // 128.87. This mirrors the vendored SwiftUITerminalHostView pattern
        // (updateSizeIfNeeded() from both layoutSubviews and updateUIView).
        uiView.updateSizeIfNeeded()
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
final class NativeTerminalModel: ObservableObject {
    @Published var isConnected = false
    @Published var statusText = "connecting to hermes..."

    private var socket: URLSessionWebSocketTask?
    private var session: URLSession?
    private var viewHandler: (([UInt8]) -> Void)?
    /// Build 128.90: weak reference to the mounted terminal view so the
    /// model can resign first responder (keyboard dismissal button).
    weak var terminalView: TerminalView?
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
    /// caller, never hardcoded.
    func start(baseURL: URL, token: String?) {
        guard socket == nil else { return }
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
        sendResize(cols: cols, rows: rows)
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
            }
        }
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
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        isConnected = false
    }

    /// Build 128.90: resign first responder on the terminal view so the
    /// software keyboard drops (manual dismiss - the Enter auto-dismiss was
    /// removed at Curtis's request).
    func dismissKeyboard() {
        terminalView?.resignFirstResponder()
    }
}

/// TerminalView subclass that actively pushes size changes into the terminal
/// on every layout pass - the same pattern SwiftTerm's own SwiftUI wrapper
/// (SwiftUITerminalHostView) uses. Without it, a TerminalView created at
/// frame .zero never learns its real bounds and renders nothing (black screen).
final class KallistiTerminalHostView: TerminalView {
    private var lastAppliedSize: CGSize = .zero

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