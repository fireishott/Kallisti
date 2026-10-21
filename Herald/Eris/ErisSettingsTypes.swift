import Foundation

// MARK: - Settings wire types
//
// Shapes mirror `crates/eris-host/src/device/handlers.rs` (`read_settings`,
// `update_settings`, `SETTINGS_FIELDS`). Both routes need the `admin` scope: a
// device without it gets `403 scope_denied` and the daemon's own sentence about
// the pairing code, which is what the settings screen shows verbatim instead of
// inventing a friendlier lie.

/// The `GET /v1/settings` snapshot.
///
/// Seven fields, none of them a secret: no key, no path, no base URL. That is
/// what makes this safe to draw on a phone somebody else can see.
struct ErisSettingsSnapshot: Equatable, Sendable {
    /// The default model id.
    let model: String
    /// `read-only`, `workspace-write` or `full`.
    let sandbox: String
    /// `gbrain`, `vault` or `off`.
    let memory: String
    /// Whether tools are switched on at all.
    let toolsEnabled: Bool
    /// How many tool rounds one turn may run.
    let toolRounds: Int
    /// `ask` or `never`.
    let approval: String
    /// The daemon's timezone label.
    let timezone: String

    init(
        model: String,
        sandbox: String,
        memory: String,
        toolsEnabled: Bool,
        toolRounds: Int,
        approval: String,
        timezone: String
    ) {
        self.model = model
        self.sandbox = sandbox
        self.memory = memory
        self.toolsEnabled = toolsEnabled
        self.toolRounds = toolRounds
        self.approval = approval
        self.timezone = timezone
    }

    /// The model id, or the honest placeholder when the daemon has none.
    var displayModel: String {
        let trimmed = model.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "no model chosen" : trimmed
    }
}

extension ErisSettingsSnapshot: Decodable {
    private enum CodingKeys: String, CodingKey {
        case model
        case sandbox
        case memory
        case toolsEnabled = "tools_enabled"
        case toolRounds = "tool_rounds"
        case approval
        case timezone
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            model: try container.decodeIfPresent(String.self, forKey: .model) ?? "",
            sandbox: try container.decodeIfPresent(String.self, forKey: .sandbox) ?? "",
            memory: try container.decodeIfPresent(String.self, forKey: .memory) ?? "",
            toolsEnabled: try container.decodeIfPresent(Bool.self, forKey: .toolsEnabled) ?? false,
            toolRounds: try container.decodeIfPresent(Int.self, forKey: .toolRounds) ?? 0,
            approval: try container.decodeIfPresent(String.self, forKey: .approval) ?? "",
            timezone: try container.decodeIfPresent(String.self, forKey: .timezone) ?? ""
        )
    }
}

/// The `POST /v1/settings` body, as the subset that actually changed.
///
/// Four fields only, because the route accepts exactly
/// `["model", "sandbox", "memory", "approval"]`. A field that still holds its
/// stored value is left out rather than echoed: the daemon answers `changed`
/// with differences, so sending a value nobody touched would make that answer
/// disagree with the request.
struct ErisSettingsPatch: Equatable, Sendable {
    /// The default model, when it changed.
    var model: String?
    /// The sandbox mode, when it changed.
    var sandbox: String?
    /// The memory backend, when it changed.
    var memory: String?
    /// The approval policy, when it changed.
    var approval: String?

    init(model: String? = nil, sandbox: String? = nil, memory: String? = nil, approval: String? = nil) {
        self.model = model
        self.sandbox = sandbox
        self.memory = memory
        self.approval = approval
    }

    /// Whether there is anything to send at all.
    var isEmpty: Bool {
        fields.isEmpty
    }

    /// The form fields for this patch, which is what `POST /v1/settings` reads.
    ///
    /// `ErisClient.formEncode` sorts the keys, so the encoded body is stable
    /// across calls and a test can assert on the exact string.
    var fields: [String: String] {
        var fields: [String: String] = [:]
        if let model, !model.isEmpty { fields["model"] = model }
        if let sandbox, !sandbox.isEmpty { fields["sandbox"] = sandbox }
        if let memory, !memory.isEmpty { fields["memory"] = memory }
        if let approval, !approval.isEmpty { fields["approval"] = approval }
        return fields
    }

    /// The field names this patch will change, in the daemon's own order.
    var changedKeys: [String] {
        var keys: [String] = []
        if model != nil { keys.append("model") }
        if sandbox != nil { keys.append("sandbox") }
        if memory != nil { keys.append("memory") }
        if approval != nil { keys.append("approval") }
        return keys
    }

    /// Diff a draft against what the daemon already holds.
    ///
    /// Only fields that differ become part of the patch, which is the whole point
    /// of the route taking a subset. `model` is compared trimmed, because the
    /// daemon trims the value it validates.
    static func changes(
        from current: ErisSettingsSnapshot,
        model: String,
        sandbox: String,
        memory: String,
        approval: String
    ) -> ErisSettingsPatch {
        var patch = ErisSettingsPatch()
        let draftModel = model.trimmingCharacters(in: .whitespacesAndNewlines)
        if !draftModel.isEmpty, draftModel != current.model.trimmingCharacters(in: .whitespacesAndNewlines) {
            patch.model = draftModel
        }
        if !sandbox.isEmpty, sandbox != current.sandbox {
            patch.sandbox = sandbox
        }
        if !memory.isEmpty, memory != current.memory {
            patch.memory = memory
        }
        if !approval.isEmpty, approval != current.approval {
            patch.approval = approval
        }
        return patch
    }
}

/// The `POST /v1/settings` answer: which fields the daemon actually wrote.
struct ErisSettingsUpdate: Equatable, Sendable, Decodable {
    /// The fields that changed, in the order the request was read.
    let changed: [String]

    init(changed: [String]) {
        self.changed = changed
    }

    private enum CodingKeys: String, CodingKey {
        case changed
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(changed: try container.decodeIfPresent([String].self, forKey: .changed) ?? [])
    }
}

// MARK: - Closed value sets

/// The values each editable setting accepts, from the daemon's own parsers.
///
/// These are literals on purpose: a picker needs a list before the daemon has
/// been asked anything, and a value outside one of these is refused by the
/// daemon with a message that names the set. The lists match
/// `SandboxMode::as_str`, `ApprovalPolicy::as_str` and `Backend::as_str`.
enum ErisSettingsOption {
    /// `SandboxMode::ALL`, in display order.
    static let sandbox: [String] = ["read-only", "workspace-write", "full"]
    /// `ApprovalPolicy::ALL`.
    static let approval: [String] = ["ask", "never"]
    /// `Backend::ALL`.
    static let memory: [String] = ["gbrain", "vault", "off"]

    /// The label a person reads for one sandbox value.
    static func sandboxLabel(_ value: String) -> String {
        switch value {
        case "read-only": return "Read only"
        case "workspace-write": return "Workspace write"
        case "full": return "Full host"
        default: return value
        }
    }

    /// The label a person reads for one approval value.
    static func approvalLabel(_ value: String) -> String {
        switch value {
        case "ask": return "Ask me"
        case "never": return "Never ask"
        default: return value
        }
    }

    /// The label a person reads for one memory backend.
    static func memoryLabel(_ value: String) -> String {
        switch value {
        case "gbrain": return "gbrain"
        case "vault": return "vault"
        case "off": return "Off"
        default: return value
        }
    }

    /// One line of plain English: what changes if this is picked.
    static func sandboxConsequence(_ value: String) -> String {
        switch value {
        case "read-only":
            return "Turns can read the workspace but cannot write a file, run a command or open a socket."
        case "workspace-write":
            return "Turns can create and edit files inside the workspace, and nothing outside it."
        case "full":
            return "Turns run on the host itself. This needs the daemon restarted with --allow-full."
        default:
            return ""
        }
    }

    /// One line of plain English for the approval policy.
    static func approvalConsequence(_ value: String) -> String {
        switch value {
        case "ask":
            return "Every action that would leave the sandbox waits for approval."
        case "never":
            return "Actions inside the sandbox run without asking. Needs a paired device and a finished setup."
        default:
            return ""
        }
    }
}
