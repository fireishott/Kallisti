import Foundation

/// `/v1/talk/readiness` payload. The connector returns this flat; older
/// client code expected a `{"data": …}` wrapper. Accept both.
struct TalkReadinessResponse: Decodable, Equatable {
    let ready: Bool
    let hostOnline: Bool?
    let configured: Bool?
    let blockedReason: String?
    let stt: String?
    let tts: String?

    static func parse(_ data: Data) throws -> TalkReadinessResponse {
        struct Wrapped: Decodable { let data: TalkReadinessResponse }
        let decoder = JSONDecoder()
        if let wrapped = try? decoder.decode(Wrapped.self, from: data) { return wrapped.data }
        return try decoder.decode(TalkReadinessResponse.self, from: data)
    }
}
