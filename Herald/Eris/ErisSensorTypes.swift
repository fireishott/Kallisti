import Foundation

// MARK: - Eris sensor / health wire types
//
// The upload shapes mirror the legacy connector payloads exactly, so the daemon
// being built to accept them sees the same JSON the old relay forwarded. The read
// shapes match the contract the daemon is being built to serve.

// MARK: Upload payloads (camelCase, exactly the legacy shapes)

/// `POST /v1/device/sensor/location` body.
struct ErisSensorLocationPayload: Encodable, Sendable {
    let latitude: Double
    let longitude: Double
    let altitude: Double?
    let accuracy: Double
    let address: String?
    let recordedAt: String
}

/// `POST /v1/device/sensor/health` body.
struct ErisSensorHealthPayload: Encodable, Sendable {
    struct Sample: Encodable, Sendable {
        let metric: String
        let value: Double
        let unit: String
        let startAt: String
        let endAt: String?
    }

    let samples: [Sample]
}

// MARK: Delivery acknowledgement

/// The `{deliveryState: "delivered"}` acknowledgement both upload routes return.
struct ErisDeliveryResult: Decodable, Sendable {
    let deliveryState: String

    var wasDelivered: Bool {
        deliveryState == "delivered"
    }
}

// MARK: Read responses (snake_case `recorded_at`)

/// `GET /v1/device/health/latest` body.
struct ErisHealthLatest: Decodable, Sendable {
    let metrics: [ErisHealthMetric]
}

/// One latest metric row.
struct ErisHealthMetric: Decodable, Sendable, Identifiable {
    let metric: String
    let value: Double
    let unit: String
    let recordedAt: String

    var id: String { metric }

    private enum CodingKeys: String, CodingKey {
        case metric
        case value
        case unit
        case recordedAt = "recorded_at"
    }
}

/// `GET /v1/device/location` body.
struct ErisLocationSnapshot: Decodable, Sendable {
    let latitude: Double
    let longitude: Double
    let altitude: Double?
    let accuracy: Double?
    let address: String?
    let recordedAt: String

    private enum CodingKeys: String, CodingKey {
        case latitude
        case longitude
        case altitude
        case accuracy
        case address
        case recordedAt = "recorded_at"
    }
}
