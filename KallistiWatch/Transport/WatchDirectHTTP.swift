//
//  WatchDirectHTTP.swift
//  KallistiWatch
//
//  The direct path: the watch talks to the daemon over the origin the phone
//  handed it. There is no Tailscale on watchOS, so this is the fallback that
//  works only when the wrist shares the host's network - and it is the only
//  path that can stream a turn end to end today.
//

import Foundation

/// One daemon, reached over HTTP.
struct WatchDirectHTTP: WatchTransport {
    let origin: URL
    let token: String
    var session: URLSession = .shared
    /// Called once when the daemon answers 401, and once only: a second 401 is
    /// the caller's `reauth`, not another refresh.
    var refresh: (@Sendable () async -> String?)? = nil

    var name: TransportName { .direct }

    func pair(deviceName: String) async throws -> WatchPairing {
        // Pairing is the phone's job: only it holds a token the daemon will take
        // a new device from.
        throw WatchTransportError.refused(status: 501, code: "pair_from_phone")
    }

    func sendTurn(text: String, conversationId: UUID?) async throws -> String {
        var form = "text=\(Self.escape(text))&source=watch"
        if let conversationId {
            form += "&conversation=\(conversationId.uuidString.lowercased())"
        }
        let body = try await post("/v1/turns", form: form)
        guard let job = body["job_id"] as? String else {
            throw WatchTransportError.refused(status: 500, code: "no_job_id")
        }
        return job
    }

    func cancel(jobID: String) async throws {
        _ = try await post("/v1/turns/\(jobID)/cancel", form: "")
    }

    func speak(text: String, voice: String?) async throws -> Data {
        var form = "text=\(Self.escape(text))"
        if let voice { form += "&voice=\(Self.escape(voice))" }
        var request = try makeRequest(path: "/v1/speak", method: "POST")
        request.httpBody = Data(form.utf8)
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        let (data, _) = try await send(request)
        return data
    }

    func transcribe(audio: Data) async throws -> String {
        var request = try makeRequest(path: "/v1/transcribe", method: "POST")
        request.httpBody = audio
        request.setValue("audio/wav", forHTTPHeaderField: "Content-Type")
        let (data, _) = try await send(request)
        let object = try? JSONSerialization.jsonObject(with: data)
        guard let text = (object as? [String: Any])?["text"] as? String else {
            throw WatchTransportError.refused(status: 500, code: "no_text")
        }
        return text
    }

    func events(jobID: String, audio: Bool) -> AsyncThrowingStream<WatchFrame, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    var path = "/v1/turns/\(jobID)/events"
                    if audio { path += "?audio=1" }
                    var request = try makeRequest(path: path, method: "GET")
                    request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
                    // A turn that already settled replays from its first frame,
                    // which is what a wrist opening late needs.
                    request.setValue("0", forHTTPHeaderField: "Last-Event-ID")
                    let (bytes, response) = try await session.bytes(for: request)
                    try Self.check(response)
                    var event = ""
                    var data = ""
                    for try await line in bytes.lines {
                        if line.hasPrefix("event: ") {
                            event = String(line.dropFirst(7))
                        } else if line.hasPrefix("data: ") {
                            data = String(line.dropFirst(6))
                        } else if line.isEmpty {
                            if let frame = Self.frame(event: event, data: data) {
                                continuation.yield(frame)
                            }
                            event = ""
                            data = ""
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    // MARK: - Wire helpers

    /// One `event`/`data` pair, as a frame the screen understands.
    static func frame(event: String, data: String) -> WatchFrame? {
        guard let object = try? JSONSerialization.jsonObject(with: Data(data.utf8)),
              let dict = object as? [String: Any] else {
            return nil
        }
        switch event {
        case "turn.start":
            return .start(source: dict["source"] as? String ?? "phone")
        case "delta":
            return .delta(text: dict["text"] as? String ?? "")
        case "tool", "tool_start":
            return .tool(name: dict["name"] as? String ?? "tool")
        case "speak.clause":
            let wav = (dict["b64"] as? String).flatMap { Data(base64Encoded: $0) } ?? Data()
            return .clause(
                index: dict["index"] as? Int ?? 0,
                wav: wav,
                text: dict["text"] as? String ?? ""
            )
        case "done":
            return .done(text: dict["text"] as? String ?? "")
        case "error":
            return .failed(
                code: dict["code"] as? String ?? "error",
                message: dict["message"] as? String ?? ""
            )
        case "cancelled":
            return .cancelled
        default:
            return nil
        }
    }

    private func makeRequest(path: String, method: String) throws -> URLRequest {
        guard let url = URL(string: path, relativeTo: origin) else {
            throw WatchTransportError.unreachable
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 120
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return request
    }

    private func post(_ path: String, form: String) async throws -> [String: Any] {
        var request = try makeRequest(path: path, method: "POST")
        request.httpBody = Data(form.utf8)
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        let (data, response) = try await send(request)
        let object = try? JSONSerialization.jsonObject(with: data)
        return (object as? [String: Any]) ?? [:]
    }

    /// One request, with the single refresh the daemon's 401 is allowed.
    private func send(_ request: URLRequest) async throws -> (Data, URLResponse) {
        let (data, response) = try await session.data(for: request)
        if let http = response as? HTTPURLResponse, http.statusCode == 401, let refresh {
            guard let fresh = await refresh() else {
                throw WatchAuthError.reauth
            }
            var retried = request
            retried.setValue("Bearer \(fresh)", forHTTPHeaderField: "Authorization")
            let (retryData, retryResponse) = try await session.data(for: retried)
            try Self.check(retryResponse)
            return (retryData, retryResponse)
        }
        try Self.check(response)
        return (data, response)
    }

    /// Trade a refresh token for a new pairing. Unauthenticated on purpose: the
    /// refresh token is the credential.
    static func refreshPairing(
        origin: URL,
        refreshToken: String,
        session: URLSession = .shared
    ) async throws -> (accessToken: String, refreshToken: String) {
        guard let url = URL(string: "/v1/token/refresh", relativeTo: origin) else {
            throw WatchTransportError.unreachable
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 30
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["refresh_token": refreshToken])
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            throw WatchAuthError.refreshRefused(status: status)
        }
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let access = object["access_token"] as? String,
              let refresh = object["refresh_token"] as? String else {
            throw WatchTransportError.refused(status: 500, code: "refresh_shape")
        }
        return (access, refresh)
    }

    private static func check(_ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse else {
            throw WatchTransportError.unreachable
        }
        guard (200..<300).contains(http.statusCode) else {
            throw WatchTransportError.refused(status: http.statusCode, code: nil)
        }
    }

    private static func escape(_ value: String) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
    }
}
