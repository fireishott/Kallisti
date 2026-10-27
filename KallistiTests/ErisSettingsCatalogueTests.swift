import Foundation
import Testing
@testable import Kallisti

// MARK: - What these hold
//
// Kallisti's Settings parity with the daemon's web UI is not "write six screens".
// `GET /v1/settings` carries `capabilities`: every operator-facing setting, with
// its label, its tab, its type, the value it currently reads and the closed list
// it accepts, in the daemon's own tab order. The phone draws that record.
//
// The rule these tests keep: the phone never invents a field, never invents an
// option, and never invents a range. What the daemon says is what a person sees
// and picks from. The one thing the client decides is which control a type
// becomes, and an unknown type must not take the screen down with it.

// MARK: - Fixtures

/// A record shaped exactly as the daemon writes it: one entry per kind, in the
/// daemon's tab order, with `value` a string for every kind.
private let catalogueJSON = #"""
{
  "ok": true,
  "model": "deepseek/deepseek-v4-flash",
  "sandbox": "workspace-write",
  "memory": "vault",
  "tools_enabled": true,
  "tool_rounds": 8,
  "approval": "ask",
  "timezone": "America/Indiana/Indianapolis",
  "clock_format": "24h",
  "push": {"notify_on_reply": true, "scope": "all", "devices": ""},
  "capabilities": [
    {"key":"model","label":"Model","group":"models","type":"string","writable":true,
     "value":"deepseek/deepseek-v4-flash","options":null},
    {"key":"tools_enabled","label":"Tools","group":"models","type":"bool","writable":true,
     "value":"true","options":null},
    {"key":"tool_rounds","label":"Tool turns","group":"models","type":"int","writable":true,
     "value":"8","options":null,"min":1,"max":16},
    {"key":"assistant_name","label":"Assistant name","group":"assistant","type":"string",
     "writable":true,"value":"Ignyte","options":null},
    {"key":"show_tool_log","label":"Run log","group":"assistant","type":"enum","writable":true,
     "value":"full","options":["full","compact","off"]},
    {"key":"memory","label":"Memory","group":"memory","type":"enum","writable":true,
     "value":"vault","options":["gbrain","vault","off"]},
    {"key":"push_notify_scope","label":"Notify scope","group":"devices","type":"enum","writable":true,
     "value":"all","options":["all","mentions"]},
    {"key":"schedules_enabled","label":"Schedules","group":"automation","type":"bool","writable":true,
     "value":"false","options":null},
    {"key":"sandbox","label":"Sandbox","group":"system","type":"enum","writable":true,
     "value":"workspace-write","options":["read-only","workspace-write","full"]},
    {"key":"workspace_path","label":"Workspace","group":"system","type":"string","writable":false,
     "value":"/home/fihadmin/kallisti-workspace","options":null}
  ]
}
"""#

private func snapshot() throws -> ErisSettingsSnapshot {
    try JSONDecoder().decode(ErisSettingsSnapshot.self, from: Data(catalogueJSON.utf8))
}

// MARK: - Tests

@Suite("Eris settings catalogue")
struct ErisSettingsCatalogueTests {

    /// The record is the screen: every entry arrives with what it takes to draw
    /// one row, and the legacy seven fields still decode for older callers.
    @Test("the record decodes into the daemon's tabs")
    func recordDecodes() throws {
        let settings = try snapshot()
        #expect(settings.fields.count == 10)
        #expect(settings.model == "deepseek/deepseek-v4-flash")
        #expect(settings.toolRounds == 8)

        let groups = settings.groups
        #expect(groups.map(\.id) == ["models", "assistant", "memory", "devices", "automation", "system"])
        #expect(groups.map(\.title) == ["Models", "Assistant", "Memory", "Devices", "Automation", "System"])
        #expect(groups[0].fields.count == 3)
        #expect(groups[5].fields.count == 2)
        // Nothing is dropped and nothing is invented.
        #expect(groups.flatMap(\.fields).count == settings.fields.count)
    }

    /// Each type becomes the control it describes, and the values come from the
    /// record rather than from a list in this app.
    @Test("each kind carries what its control needs")
    func kindsCarryTheirEvidence() throws {
        let settings = try snapshot()

        let tools = try #require(settings.field("tools_enabled"))
        #expect(tools.kind == .bool)
        #expect(tools.boolValue)

        let rounds = try #require(settings.field("tool_rounds"))
        #expect(rounds.kind == .int)
        #expect(rounds.min == 1)
        #expect(rounds.max == 16)
        #expect(rounds.intValue == 8)

        let runLog = try #require(settings.field("show_tool_log"))
        #expect(runLog.kind == .enumeration)
        #expect(runLog.options == ["full", "compact", "off"])
        #expect(runLog.selectedOption == "full")
        #expect(runLog.label(for: "compact") == "Compact")

        let name = try #require(settings.field("assistant_name"))
        #expect(name.kind == .string)
        #expect(name.value == "Ignyte")

        // A field the daemon does not accept writes for says so, rather than
        // offering a control that would fail.
        let workspace = try #require(settings.field("workspace_path"))
        #expect(!workspace.writable)
    }

    /// A record entry this build cannot draw is dropped, and every other row
    /// still renders: one unknown type is not an empty settings screen.
    @Test("an unknown type costs one row, not the screen")
    func unknownKindIsSkipped() throws {
        let body = #"""
        {"ok":true,"capabilities":[
          {"key":"model","label":"Model","group":"models","type":"string","writable":true,"value":"m"},
          {"key":"future_thing","label":"Future","group":"system","type":"duration","writable":true,"value":"5m"},
          {"key":"workspace_path","label":"Workspace","group":"system","type":"string","writable":false,"value":"/tmp"}
        ]}
        """#
        let settings = try JSONDecoder().decode(ErisSettingsSnapshot.self, from: Data(body.utf8))
        #expect(settings.fields.map(\.key) == ["model", "workspace_path"])
    }

    /// A write is one key and one value, spelled the way the daemon's parser
    /// wants: a flag is lower case, a number is clamped to the published range,
    /// a choice must be one of the offered options, and empty text is refused
    /// rather than stored as blank.
    @Test("a write spells each kind the way the daemon reads it")
    func writesSpellTheirValues() throws {
        let settings = try snapshot()

        #expect(ErisSettingWrite.flag("tools_enabled", true).fields == ["tools_enabled": "true"])
        #expect(ErisSettingWrite.flag("schedules_enabled", false).fields == ["schedules_enabled": "false"])

        let rounds = try #require(settings.field("tool_rounds"))
        #expect(ErisSettingWrite.number(rounds.key, 99, min: rounds.min, max: rounds.max).value == "16")
        #expect(ErisSettingWrite.number(rounds.key, 0, min: rounds.min, max: rounds.max).value == "1")
        #expect(ErisSettingWrite.number(rounds.key, 4, min: rounds.min, max: rounds.max).value == "4")

        let runLog = try #require(settings.field("show_tool_log"))
        #expect(ErisSettingWrite.choice(runLog, "compact")?.value == "compact")
        // A value the daemon does not offer is refused here, not sent to be
        // refused there.
        #expect(ErisSettingWrite.choice(runLog, "verbose") == nil)

        #expect(ErisSettingWrite.text("assistant_name", "  Ignyte  ")?.value == "Ignyte")
        #expect(ErisSettingWrite.text("assistant_name", "   ") == nil)
    }

    /// Every daemon tab the web UI renders has a field on the phone, so no tab
    /// can be silently empty.
    @Test("all six daemon tabs are populated")
    func everyTabHasFields() throws {
        let groups = try snapshot().groups
        for group in ["models", "assistant", "memory", "devices", "automation", "system"] {
            let match = groups.first { $0.id == group }
            #expect(match != nil, "\(group) has a tab")
            #expect((match?.fields.isEmpty == false), "\(group) has at least one field")
        }
    }
}

// MARK: - The switch that gates `full`
//
// `sandbox=full` is refused until the daemon's full-access switch is on, and that
// switch deliberately is not in the capability record: enabling it needs the word
// typed and disabling it demotes a `full` sandbox with it. So it has its own route
// and its own state, and these tests hold the two things that matter about it: the
// switch's state comes from the daemon, and the answer to a flip is the state
// after it rather than a copy of the request.

@Suite("Eris full access switch")
struct ErisFullAccessTests {

    @Test("the snapshot says whether the switch is on")
    func snapshotCarriesTheSwitch() throws {
        let on = #"{"ok":true,"full_access":true,"sandbox":"full","capabilities":[]}"#
        let off = #"{"ok":true,"full_access":false,"sandbox":"workspace-write","capabilities":[]}"#
        #expect(try JSONDecoder().decode(ErisSettingsSnapshot.self, from: Data(on.utf8)).fullAccess)
        let disabled = try JSONDecoder().decode(ErisSettingsSnapshot.self, from: Data(off.utf8))
        #expect(!disabled.fullAccess)
        // A daemon that predates the key reads as off, not as broken.
        let older = #"{"ok":true,"sandbox":"read-only","capabilities":[]}"#
        #expect(try JSONDecoder().decode(ErisSettingsSnapshot.self, from: Data(older.utf8)).fullAccess == false)
    }

    /// The two host facts the Devices tab holds. A phone shows them and cannot
    /// move them: a bind cannot be changed under a live phone, so the surface with
    /// the address in its hand is the one that must not be able to rewrite it.
    @Test("the snapshot carries where the daemon is reachable")
    func snapshotCarriesReachability() throws {
        let body = #"{"ok":true,"listen":"127.0.0.1:7341, 192.168.68.51:7341","relay":"https://kallisti.example.test","capabilities":[]}"#
        let settings = try JSONDecoder().decode(ErisSettingsSnapshot.self, from: Data(body.utf8))
        #expect(settings.listen == "127.0.0.1:7341, 192.168.68.51:7341")
        #expect(settings.relay == "https://kallisti.example.test")

        // A daemon that predates the two keys, and one with no origin set, are
        // both readable rather than broken.
        let older = #"{"ok":true,"capabilities":[]}"#
        let bare = try JSONDecoder().decode(ErisSettingsSnapshot.self, from: Data(older.utf8))
        #expect(bare.listen.isEmpty)
        #expect(bare.relay.isEmpty)
    }

    /// Disabling demotes a `full` sandbox, so the result has to carry the sandbox
    /// the daemon holds now. A surface that only heard its own request would keep
    /// drawing a mode that is no longer selected.
    @Test("the answer is the state after the flip")
    func resultCarriesTheDemotedSandbox() throws {
        let body = #"{"ok":true,"full_access":false,"sandbox":"workspace-write"}"#
        let result = try JSONDecoder().decode(ErisFullAccessResult.self, from: Data(body.utf8))
        #expect(!result.fullAccess)
        #expect(result.sandbox == "workspace-write")
    }
}
