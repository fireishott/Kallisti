import Foundation

// MARK: - Pairing

/// A pairing the app can act on: where the daemon is, and the code to redeem.
///
/// The QR payload is compact JSON, `{"code":"...","relay":"http://host:7341"}`.
/// `relay` is the device listener's base URL (`127.0.0.1:7341` on the daemon
/// host, or whatever address it is bound to); `code` is the single-use pairing
/// code. A manually typed base URL and code produce the same value, so the
/// pairing sheet and the scanner hand the transport one type.
struct ErisPairing: Equatable, Sendable {
    /// The single-use pairing code.
    let code: String
    /// The daemon's device listener base URL, e.g. `http://192.168.1.20:7341`.
    let baseURL: URL

    /// Decode a scanned QR payload.
    ///
    /// - Parameter payload: the raw string the scanner read.
    /// - Throws: `ErisPairingError.notJSON` for anything that is not a JSON
    ///   object, `missingCode`/`missingRelay` when a required key is absent, and
    ///   `invalidCode`/`invalidRelay` when a value is present but unusable.
    init(payload: String) throws {
        let trimmed = payload.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { throw ErisPairingError.notJSON }
        guard let data = trimmed.data(using: .utf8) else { throw ErisPairingError.notJSON }
        let parsed: Any
        do {
            parsed = try JSONSerialization.jsonObject(with: data)
        } catch {
            throw ErisPairingError.notJSON
        }
        guard let object = parsed as? [String: Any] else { throw ErisPairingError.notJSON }

        guard let rawCode = object["code"] else { throw ErisPairingError.missingCode }
        guard let rawRelay = object["relay"] else { throw ErisPairingError.missingRelay }
        // A key that is present but not a string is the same problem as a key
        // that is absent: nothing usable is there. Both answer the typed error.
        guard let code = rawCode as? String else { throw ErisPairingError.missingCode }
        guard let relay = rawRelay as? String else { throw ErisPairingError.missingRelay }

        try self.init(baseURLString: relay, code: code)
    }

    /// Build a pairing from a manually typed base URL and code.
    init(baseURLString: String, code: String) throws {
        let trimmedCode = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedCode.isEmpty else { throw ErisPairingError.invalidCode }

        let trimmedRelay = baseURLString.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedRelay.isEmpty else { throw ErisPairingError.missingRelay }
        guard let url = URL(string: trimmedRelay) else {
            throw ErisPairingError.invalidRelay(baseURLString)
        }
        // Only the two schemes the daemon listener serves. A payload naming
        // something else would build requests no URLSession can send.
        guard let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https" else {
            throw ErisPairingError.invalidRelay(baseURLString)
        }
        guard let host = url.host, !host.isEmpty else {
            throw ErisPairingError.invalidRelay(baseURLString)
        }

        self.code = trimmedCode
        self.baseURL = url
    }
}

/// Why a pairing payload could not be used.
///
/// One case per rejection so the pairing sheet can say which field is wrong
/// instead of "invalid QR code".
enum ErisPairingError: Error, Equatable, Sendable, LocalizedError {
    /// The payload is not a JSON object.
    case notJSON
    /// The payload has no `code`.
    case missingCode
    /// The payload has no `relay`.
    case missingRelay
    /// `code` is empty after trimming.
    case invalidCode
    /// `relay` is not an http(s) URL with a host.
    case invalidRelay(String)

    var errorDescription: String? {
        switch self {
        case .notJSON:
            return "that QR code is not a Kallisti pairing payload"
        case .missingCode:
            return "the pairing payload has no code"
        case .missingRelay:
            return "the pairing payload has no relay address"
        case .invalidCode:
            return "the pairing code is empty"
        case .invalidRelay(let value):
            return "\(value) is not an http or https address"
        }
    }
}
