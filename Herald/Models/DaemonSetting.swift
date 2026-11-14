import Foundation

// The daemon's capability record, as the phone reads it.
//
// The daemon publishes one grouped, typed record and validates every write
// against the same table it built the record from. That means the phone does not
// know any field: it draws what arrives, and a field added to the daemon appears
// here with no client change. These types are the wire shape only.
//
// `type` is a closed set of four on purpose. An unknown value renders read-only
// with the raw string rather than crashing, so a daemon that grows a fifth kind
// renders its new field badly instead of not at all.

/// One group of settings: the six the daemon owns, in its own order.
///
/// The titles are the daemon's group labels, title-cased, and the symbols are the
/// only client-side constants in this feature.
enum DaemonGroup: String, CaseIterable, Identifiable, Hashable {
    case models
    case assistant
    case memory
    case devices
    case automation
    case system

    var id: String { rawValue }

    /// The word on the row and the title of the screen.
    var title: String {
        switch self {
        case .models: return "Models"
        case .assistant: return "Assistant"
        case .memory: return "Memory"
        case .devices: return "Devices"
        case .automation: return "Automation"
        case .system: return "System"
        }
    }

    /// The line under the title.
    var blurb: String {
        switch self {
        case .models: return "Where a turn is sent, and what it may call."
        case .assistant: return "How a turn talks, and how much it says."
        case .memory: return "What a turn recalls, and how much it may keep."
        case .devices: return "Where this phone reaches the daemon, and what it is told."
        case .automation: return "What runs without anyone asking."
        case .system: return "What a turn may touch, and how the daemon looks."
        }
    }

    /// One symbol per group, chosen once and reused.
    var symbol: String {
        switch self {
        case .models: return "cpu"
        case .assistant: return "text.bubble"
        case .memory: return "brain"
        case .devices: return "iphone"
        case .automation: return "clock.arrow.circlepath"
        case .system: return "gearshape.2"
        }
    }

    /// Where the group's order comes from: the daemon's own list, read as an
    /// index. An unknown group string still renders, at the end.
    static func order(of raw: String) -> Int {
        DaemonGroup(rawValue: raw).flatMap { allCases.firstIndex(of: $0) } ?? allCases.count
    }
}

/// What kind of control a field takes.
enum DaemonSettingKind: String {
    case bool
    case int
    case string
    case enumeration = "enum"
    /// A `type` this build has never seen. Rendered read-only, with the value.
    case unknown
}

/// One setting the daemon publishes.
struct DaemonSetting: Identifiable, Hashable {
    let key: String
    let label: String
    let group: String
    let rawType: String
    let value: String
    let writable: Bool
    let options: [String]?
    let minimum: Int?
    let maximum: Int?

    var id: String { key }

    var kind: DaemonSettingKind {
        guard writable else { return .unknown }
        return DaemonSettingKind(rawValue: rawType) ?? .unknown
    }

    /// The label a picker shows for one option. The daemon's own spellings are
    /// the values; the words are the reader's.
    static func optionLabel(_ value: String) -> String {
        switch value {
        case "all": return "Every conversation"
        case "conversation": return "This conversation"
        case "push": return "Push"
        case "inbox": return "Inbox"
        case "email": return "Mail"
        case "off": return "Off"
        case "read-only": return "Read only"
        case "workspace-write": return "Workspace"
        case "full": return "Full access"
        case "ask": return "Ask first"
        case "never": return "Never ask"
        default: return value
        }
    }

    var intBounds: ClosedRange<Int>? {
        guard kind == .int, let minimum, let maximum, minimum <= maximum else { return nil }
        return minimum...maximum
    }
}

/// The whole record, in one read.
struct DaemonCapabilityRecord: Decodable {
    let capabilities: [DaemonSetting]

    enum CodingKeys: String, CodingKey {
        case capabilities
    }

    init(from decoder: Decoder) throws {
        let root = try decoder.container(keyedBy: CodingKeys.self)
        let raw = try root.decodeIfPresent([Raw].self, forKey: .capabilities) ?? []
        capabilities = raw.map(\.setting)
    }

    /// The wire entry. Decoded leniently field by field: one malformed row must
    /// not cost the whole screen.
    private struct Raw: Decodable {
        let key: String
        let label: String?
        let group: String?
        let type: String?
        let value: String?
        let writable: Bool?
        let options: [String]?
        let min: Int?
        let max: Int?

        enum CodingKeys: String, CodingKey {
            case key, label, group, type, value, writable, options, min, max
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            key = try c.decode(String.self, forKey: .key)
            label = try? c.decodeIfPresent(String.self, forKey: .label)
            group = try? c.decodeIfPresent(String.self, forKey: .group)
            type = try? c.decodeIfPresent(String.self, forKey: .type)
            writable = try? c.decodeIfPresent(Bool.self, forKey: .writable)
            options = try? c.decodeIfPresent([String].self, forKey: .options)
            min = try? c.decodeIfPresent(Int.self, forKey: .min)
            max = try? c.decodeIfPresent(Int.self, forKey: .max)
            // `value` arrives as a string for most kinds and as a bool or number
            // for a few. Everything is normalized to its string spelling here so
            // no control has to ask twice.
            if let text = try? c.decodeIfPresent(String.self, forKey: .value) {
                value = text
            } else if let flag = try? c.decodeIfPresent(Bool.self, forKey: .value) {
                value = flag ? "true" : "false"
            } else if let number = try? c.decodeIfPresent(Int.self, forKey: .value) {
                value = String(number)
            } else {
                value = nil
            }
        }

        var setting: DaemonSetting {
            DaemonSetting(
                key: key,
                label: label ?? key,
                group: group ?? DaemonGroup.system.rawValue,
                rawType: type ?? "string",
                value: value ?? "",
                writable: writable ?? false,
                options: options,
                minimum: min,
                maximum: max
            )
        }
    }
}

/// The refusal body the daemon answers a bad write with.
///
/// `field` is the key it refused, which is what lets the screen put the sentence
/// under the row that caused it rather than at the top of the page.
struct DaemonRefusal: Decodable {
    let field: String?
    let message: String?

    enum CodingKeys: String, CodingKey {
        case field, message
    }
}
