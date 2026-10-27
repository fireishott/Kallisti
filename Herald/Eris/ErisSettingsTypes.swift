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
    /// Where the device listener binds, as the host holds it.
    ///
    /// Read-only on the phone by design: a bind cannot be moved under a live
    /// phone, and a phone that could move its own daemon's bind is one typo away
    /// from locking itself out. Shown so the address is visible without a browser.
    let listen: String
    /// The origin a paired phone should call, when the operator has set one.
    ///
    /// This is what the pairing QR carries, so it is the address that actually
    /// routes to this daemon rather than the one the host binds for itself.
    let relay: String
    /// Whether full host access is enabled on the daemon.
    ///
    /// The one switch `capabilities` cannot carry: turning it on needs the word
    /// typed, and turning it off demotes a `full` sandbox with it, so it is not
    /// a plain setting and not a plain write. `sandbox=full` is refused until
    /// this is true, which is what made it a dead end on the phone.
    let fullAccess: Bool
    /// Every operator-facing setting the daemon holds, in its own tab order.
    ///
    /// This is the Settings screen: labels, types, current values and closed
    /// option lists all arrive here, so the phone renders the daemon's table
    /// rather than a second copy of it that goes stale the next time a setting
    /// is added.
    let fields: [ErisSettingField]

    init(
        model: String,
        sandbox: String,
        memory: String,
        toolsEnabled: Bool,
        toolRounds: Int,
        approval: String,
        timezone: String,
        listen: String = "",
        relay: String = "",
        fullAccess: Bool = false,
        fields: [ErisSettingField] = []
    ) {
        self.model = model
        self.sandbox = sandbox
        self.memory = memory
        self.toolsEnabled = toolsEnabled
        self.toolRounds = toolRounds
        self.approval = approval
        self.timezone = timezone
        self.listen = listen
        self.relay = relay
        self.fullAccess = fullAccess
        self.fields = fields
    }

    /// The catalogue, grouped into the daemon's tabs.
    var groups: [ErisSettingsGroup] {
        ErisSettingsGroup.groups(from: fields)
    }

    /// The field behind one key, when the record carries it.
    func field(_ key: String) -> ErisSettingField? {
        fields.first { $0.key == key }
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
        case listen
        case relay
        case fullAccess = "full_access"
        case capabilities
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
            timezone: try container.decodeIfPresent(String.self, forKey: .timezone) ?? "",
            listen: try container.decodeIfPresent(String.self, forKey: .listen) ?? "",
            relay: try container.decodeIfPresent(String.self, forKey: .relay) ?? "",
            fullAccess: try container.decodeIfPresent(Bool.self, forKey: .fullAccess) ?? false,
            fields: (try container.decodeIfPresent([FailableSettingField].self, forKey: .capabilities) ?? [])
                .compactMap(\.field)
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

    /// The label for one option of any field.
    ///
    /// The three lists above keep their own readers because the consequence
    /// strings beside them are written for a specific choice; everything the
    /// record grows uses this one, which falls back to the daemon's own spelling
    /// rather than inventing a word for a value it has never seen.
    static func displayLabel(key: String, value: String) -> String {
        switch key {
        case "sandbox": return sandboxLabel(value)
        case "approval": return approvalLabel(value)
        case "memory": return memoryLabel(value)
        case "clock_format":
            switch value {
            case "24h": return "24-hour"
            case "12h": return "12-hour"
            default: return value
            }
        case "show_tool_log":
            switch value {
            case "full": return "Every call"
            case "compact": return "Compact"
            case "off": return "Off"
            default: return value
            }
        case "reply_state":
            switch value {
            case "bubble": return "Bubble"
            case "inline": return "Inline"
            default: return value
            }
        case "greeting_refresh":
            switch value {
            case "morning": return "Each morning"
            case "session": return "Each session"
            case "manual": return "Manual"
            default: return value
            }
        case "push_notify_scope":
            switch value {
            case "all": return "Every turn"
            case "mentions": return "Only when named"
            default: return value
            }
        case "conversation_sort":
            switch value {
            case "recent": return "Newest first"
            case "title": return "By title"
            case "day": return "By day"
            default: return value
            }
        default:
            // `workspace-write` reads as a word when it is spelled with spaces.
            return value.replacingOccurrences(of: "-", with: " ")
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

// MARK: - The daemon's own catalogue
//
// `GET /v1/settings` carries `capabilities`: every operator-facing setting the
// daemon holds, in the daemon's own tab order, with its label, its type, the
// value it currently reads, and - for a closed set - the values it accepts.
//
// That record is the Settings screen. Building one screen per web tab by hand
// would be a second copy of a table the daemon already publishes, and the copy
// is what goes stale: a setting added to the daemon would be invisible on the
// phone until somebody remembered to add it here too.
//
// So the phone renders the record, and the only thing it decides is which
// control a type becomes.

/// What kind of value a setting holds, as the record spells it.
///
/// Four types, because that is how many form controls the record chooses from.
/// A zone is a `string` on the wire by the daemon's own choice, so it gets the
/// text field it deserves rather than a fifth case nothing else uses.
enum ErisSettingKind: String, Sendable, Equatable {
    /// Free text.
    case string
    /// One of a closed list.
    case enumeration
    /// On or off.
    case bool
    /// A whole number inside `min`...`max`.
    case int

    /// Decode the record's `type`. A kind this build does not know is `nil`, so
    /// a newer daemon's field is skipped instead of crashing the screen.
    init?(wire: String) {
        switch wire {
        case "string": self = .string
        case "enum": self = .enumeration
        case "bool": self = .bool
        case "int": self = .int
        default: return nil
        }
    }
}

/// One setting, exactly as the daemon describes it.
struct ErisSettingField: Identifiable, Equatable, Sendable {
    /// The write body's field name.
    let key: String
    /// What a person reads beside it.
    let label: String
    /// Which tab it belongs to, one of the daemon's six.
    let group: String
    /// What kind of control it becomes.
    let kind: ErisSettingKind
    /// Whether the daemon accepts a write for it today.
    let writable: Bool
    /// The value it currently reads, as the daemon spells it.
    var value: String
    /// The closed list, empty for anything but an enum.
    var options: [String] = []
    /// The lowest value an `int` accepts, when the daemon publishes one.
    var min: Int? = nil
    /// The highest value an `int` accepts, when the daemon publishes one.
    var max: Int? = nil

    var id: String { key }

    /// The stored value as a switch position.
    var boolValue: Bool { value.lowercased() == "true" }

    /// The stored value as a number, falling back to the low end of the range.
    var intValue: Int { Int(value) ?? min ?? 0 }

    /// The label for one of this field's options, when it is an enum.
    func label(for option: String) -> String {
        // The enum values are the daemon's spellings (`workspace-write`,
        // `gbrain`) and several of them read as words rather than as keys.
        ErisSettingsOption.displayLabel(key: key, value: option)
    }

    /// The option a person would pick for the stored value.
    var selectedOption: String? {
        guard kind == .enumeration else { return nil }
        if options.contains(value) { return value }
        return options.first
    }
}

extension ErisSettingField: Decodable {
    private enum CodingKeys: String, CodingKey {
        case key, label, group, type, writable, value, options, min, max
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let key = try container.decodeIfPresent(String.self, forKey: .key) ?? ""
        guard !key.isEmpty else {
            throw ErisError.decoding("a setting in the record carried no key")
        }
        let wireType = try container.decodeIfPresent(String.self, forKey: .type) ?? ""
        guard let kind = ErisSettingKind(wire: wireType) else {
            throw ErisError.decoding("the setting `\(key)` is typed `\(wireType)`, which this build cannot draw")
        }
        self.init(
            key: key,
            label: try container.decodeIfPresent(String.self, forKey: .label) ?? key,
            group: try container.decodeIfPresent(String.self, forKey: .group) ?? "system",
            kind: kind,
            writable: try container.decodeIfPresent(Bool.self, forKey: .writable) ?? false,
            // The record's `value` is a string for every kind, including bools
            // and numbers, because one reader serves all four.
            value: try container.decodeIfPresent(String.self, forKey: .value) ?? "",
            options: try container.decodeIfPresent([String].self, forKey: .options) ?? [],
            min: try container.decodeIfPresent(Int.self, forKey: .min),
            max: try container.decodeIfPresent(Int.self, forKey: .max)
        )
    }
}

/// One field, or nothing, so a record entry this build cannot draw does not take
/// the whole screen with it.
///
/// The daemon publishes four types today. A future daemon that publishes a fifth
/// is not a reason for a phone to show an empty Settings screen: the fields it
/// does understand still have to render, and the one it does not is the field the
/// person will notice is missing. The failure is in the log either way.
private struct FailableSettingField: Decodable {
    let field: ErisSettingField?

    init(from decoder: any Decoder) throws {
        field = try? ErisSettingField(from: decoder)
    }
}

/// The `POST /v1/settings/full-access` answer.
///
/// The state after the write, not an echo of the request: disabling the switch
/// also demotes a `full` sandbox, so a surface that only heard its own request
/// would keep drawing a mode the daemon no longer holds.
struct ErisFullAccessResult: Equatable, Sendable, Decodable {
    /// Whether full host access is now on.
    let fullAccess: Bool
    /// The sandbox the daemon holds now, which may have been demoted.
    let sandbox: String

    init(fullAccess: Bool, sandbox: String) {
        self.fullAccess = fullAccess
        self.sandbox = sandbox
    }

    private enum CodingKeys: String, CodingKey {
        case fullAccess = "full_access"
        case sandbox
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            fullAccess: try container.decodeIfPresent(Bool.self, forKey: .fullAccess) ?? false,
            sandbox: try container.decodeIfPresent(String.self, forKey: .sandbox) ?? ""
        )
    }
}

/// One tab of the settings screen: the daemon's group and the fields in it.
struct ErisSettingsGroup: Identifiable, Equatable, Sendable {
    /// The daemon's group key: `models`, `assistant`, `memory`, `devices`,
    /// `automation`, `system`.
    let id: String
    /// The tab's heading.
    let title: String
    /// The fields that belong to it, in the daemon's order.
    let fields: [ErisSettingField]

    /// The heading for one of the daemon's six groups.
    ///
    /// Unknown groups are titled with their own key rather than dropped: a
    /// daemon that grows a seventh tab should show it, not hide it.
    static func title(for group: String) -> String {
        switch group {
        case "models": return "Models"
        case "assistant": return "Assistant"
        case "memory": return "Memory"
        case "devices": return "Devices"
        case "automation": return "Automation"
        case "system": return "System"
        default: return group.prefix(1).uppercased() + group.dropFirst()
        }
    }

    /// Group the record, preserving the daemon's tab order.
    ///
    /// Order is the daemon's, not alphabetical and not a client-side list: the
    /// two clients have to agree about what the tabs are, and the record already
    /// says.
    static func groups(from fields: [ErisSettingField]) -> [ErisSettingsGroup] {
        var order: [String] = []
        var byGroup: [String: [ErisSettingField]] = [:]
        for field in fields {
            if byGroup[field.group] == nil { order.append(field.group) }
            byGroup[field.group, default: []].append(field)
        }
        return order.map { group in
            ErisSettingsGroup(id: group, title: title(for: group), fields: byGroup[group] ?? [])
        }
    }
}

/// One field's new value, ready for `POST /v1/settings`.
///
/// The route takes a subset of the record's keys and reads a form, so a write is
/// one `key=value` pair. `ErisSettingsPatch` stays for the four-field callers
/// that predate the record; this is the general shape.
struct ErisSettingWrite: Equatable, Sendable {
    /// The record key to write.
    let key: String
    /// The value, spelled the way the daemon's parser wants it.
    let value: String

    /// The form body. `ErisClient.formEncode` sorts keys, so one field is stable.
    var fields: [String: String] { [key: value] }

    /// The write for a switch.
    static func flag(_ key: String, _ on: Bool) -> ErisSettingWrite {
        ErisSettingWrite(key: key, value: on ? "true" : "false")
    }

    /// The write for a number, clamped into the range the daemon published.
    static func number(_ key: String, _ value: Int, min: Int?, max: Int?) -> ErisSettingWrite {
        var clamped = value
        if let min { clamped = Swift.max(min, clamped) }
        if let max { clamped = Swift.min(max, clamped) }
        return ErisSettingWrite(key: key, value: String(clamped))
    }

    /// The write for a closed list, refusing a value the field does not offer.
    static func choice(_ field: ErisSettingField, _ option: String) -> ErisSettingWrite? {
        guard field.options.contains(option) else { return nil }
        return ErisSettingWrite(key: field.key, value: option)
    }

    /// The write for free text, refusing an empty one: the daemon treats an
    /// empty value as "no setting", which is not what clearing a field means.
    static func text(_ key: String, _ raw: String) -> ErisSettingWrite? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return ErisSettingWrite(key: key, value: trimmed)
    }
}
