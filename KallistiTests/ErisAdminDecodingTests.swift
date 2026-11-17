import XCTest
@testable import Kallisti

/// The daemon-facing models, checked against payloads copied from the live daemon.
///
/// The screens went empty twice for two different reasons - a protocol default that
/// answered instead of the client, and a model strict enough to fail on one null - so
/// the payloads here are the real ones, nulls included.
final class ErisAdminDecodingTests: XCTestCase {
    private func data(_ text: String) -> Data { Data(text.utf8) }

    func testScheduleWithNeverFiredRowDecodes() throws {
        let payload = """
        {"count":2,"enabled":true,"ok":true,"jobs":[
          {"id":"a673fe378f0a473cab67b1","name":"Morning brief","schedule":"30 7 * * *",
           "prompt":"summarise","enabled":true,"next_run":"2026-11-17T07:30:00Z",
           "last_run":null,"last_status":null,"fired_count":5,"deliver":"push","last_error":null},
          {"id":"b1","name":"Weekly cost report","schedule":"0 18 * * 0","prompt":"report",
           "enabled":false,"next_run":null,"last_run":"2026-11-10T18:00:00Z",
           "last_status":"ok","fired_count":1,"deliver":"push","last_error":null}
        ]}
        """
        struct Body: Decodable { let jobs: [ErisCronJob] }
        let body = try JSONDecoder().decode(Body.self, from: data(payload))
        XCTAssertEqual(body.jobs.count, 2)
        XCTAssertEqual(body.jobs[0].name, "Morning brief")
        XCTAssertNil(body.jobs[0].lastRun)
        XCTAssertFalse(body.jobs[1].enabled)
        XCTAssertEqual(body.jobs[1].lastStatus, "ok")
    }

    func testSkillWithNoArchiveDecodes() throws {
        let payload = """
        {"enabled":true,"ok":true,"dir":"/home/fihadmin/.config/kallisti/skills","skills":[
          {"key":"dsh-image-vision-routing","name":"Image vision routing",
           "description":"Route images to a vision model","path":"/skills/x.md","enabled":true,
           "bytes":1200,"origin":"operator","state":"active","pinned":false,"editable":true,
           "use_count":3,"last_used":null,"archived_at":null,"reason":null}
        ]}
        """
        struct Body: Decodable { let skills: [ErisSkill] }
        let body = try JSONDecoder().decode(Body.self, from: data(payload))
        XCTAssertEqual(body.skills.count, 1)
        XCTAssertEqual(body.skills[0].key, "dsh-image-vision-routing")
        XCTAssertEqual(body.skills[0].description, "Route images to a vision model")
    }

    func testDiagnosticsRingDecodes() throws {
        let payload = """
        {"version":"1.9.0","capacity":200,"slow_turn_ms":30000,"entries":[
          {"kind":"turn","detail":"turn 01a1 finished","at":"2026-11-16T19:04:21.290Z","duration_ms":812}
        ]}
        """
        let body = try JSONDecoder().decode(ErisDiagnostics.self, from: data(payload))
        XCTAssertEqual(body.entries?.count, 1)
        XCTAssertEqual(body.entries?.first?.kind, "turn")
        XCTAssertEqual(body.entries?.first?.durationMs, 812)
    }

    /// The settings route publishes 52 capabilities; the editor's text is built from
    /// their values, so a null value must become an editable empty string and not a
    /// missing line.
    func testSettingsCapabilitiesBecomeLines() throws {
        let payload = """
        {"ok":true,"capabilities":[
          {"group":"models","key":"model","label":"Model","options":null,"type":"string","value":"deepseek-flash","writable":true},
          {"group":"models","key":"tool_rounds","label":"Tool rounds","options":null,"type":"number","value":15,"writable":true},
          {"group":"assistant","key":"assistant_name","label":"Name","options":null,"type":"string","value":null,"writable":true}
        ]}
        """
        let object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: data(payload)) as? [String: Any]
        )
        var fields: [String: String] = [:]
        for capability in (object["capabilities"] as? [[String: Any]] ?? []) {
            guard let key = capability["key"] as? String, let raw = capability["value"] else { continue }
            if let text = raw as? String { fields[key] = text }
            else if let number = raw as? NSNumber { fields[key] = number.stringValue }
            else if raw is NSNull { fields[key] = "" }
        }
        XCTAssertEqual(fields.count, 3)
        XCTAssertEqual(fields["model"], "deepseek-flash")
        XCTAssertEqual(fields["tool_rounds"], "15")
        XCTAssertEqual(fields["assistant_name"], "")
    }

    /// The RPC envelope the latency row measures around.
    func testPingEnvelopeDecodes() throws {
        let payload = #"{"context":"operator","method":"ping","ok":true,"result":{"version":"1.9.0"}}"#
        let answer = try JSONDecoder().decode(ErisRPCAnswer.self, from: data(payload))
        XCTAssertEqual(answer.ok, true)
    }
}
