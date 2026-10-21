import Foundation

// MARK: - Approvals wire types
//
// Shapes mirror `crates/eris-host/src/device/handlers.rs` (the approvals list and
// the decide route). Both need the `admin` scope, so a device paired without the
// administration box gets `403 scope_denied` and the daemon's own sentence about
// the pairing code. The screen shows that sentence rather than inventing one.

/// One call parked for a person to answer.
struct ErisApproval: Equatable, Sendable, Identifiable {
    /// The approval id, which is what a decision names.
    let id: String
    /// The tool that was asked for.
    let tool: String
    /// Its arguments, already rendered as one line for reading.
    let arguments: String
    /// Why the daemon thought it worth a look, when the approval role said
    /// something. Nil when no advice model is pinned.
    let reason: String?
    /// When it was parked.
    let createdAt: String
    /// When it stops mattering. The daemon has no expiry, so this is nil today;
    /// the field exists because the wire has it.
    let expiresAt: String?
}

/// The `GET /v1/approvals` page.
struct ErisApprovalPage: Equatable, Sendable {
    /// The rows on this page, newest first.
    let approvals: [ErisApproval]
    /// How many rows this page carries.
    let count: Int
    /// Whether the daemon holds more than this page.
    let truncated: Bool
}

/// What a decision is called on the wire.
enum ErisApprovalDecision: String, Sendable {
    /// Run it.
    case approved
    /// Never run it.
    case denied

    /// The word shown on the button.
    var title: String {
        switch self {
        case .approved: return "Allow"
        case .denied: return "Deny"
        }
    }
}

/// The daemon's answer to a decision.
///
/// `result` is present only on an approval that ran: the daemon executes the call
/// and reports what it said, so a person sees the consequence rather than only
/// that a button was pressed.
struct ErisApprovalOutcome: Equatable, Sendable {
    /// The row that was settled.
    let id: String
    /// What it was settled as.
    let decision: String
    /// What the call said, when it ran.
    let result: String?
}

// MARK: - Decoding

/// Decode the approvals page.
///
/// `arguments` arrives as JSON rather than as a string, because the daemon emits
/// the call so a client can show it. Anything unreadable falls back to its own
/// text instead of an empty box: a person deciding on a write should see the write.
extension ErisApprovalPage {
    /// Read one page out of the daemon's JSON.
    ///
    /// # Throws
    ///
    /// `ErisError.decoding` when the payload is not the documented shape.
    static func decode(_ data: Data) throws -> ErisApprovalPage {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw ErisError.decoding("the approvals page is not a JSON object")
        }
        let rows = object["approvals"] as? [[String: Any]] ?? []
        let approvals: [ErisApproval] = rows.compactMap { row in
            guard let id = row["id"] as? String, let tool = row["tool"] as? String else {
                return nil
            }
            return ErisApproval(
                id: id,
                tool: tool,
                arguments: ErisApprovalPage.renderArguments(row["arguments"]),
                reason: row["reason"] as? String,
                createdAt: row["created_at"] as? String ?? "",
                expiresAt: row["expires_at"] as? String
            )
        }
        return ErisApprovalPage(
            approvals: approvals,
            count: object["count"] as? Int ?? approvals.count,
            truncated: object["truncated"] as? Bool ?? false
        )
    }

    /// One line of arguments, whatever shape the daemon sent.
    static func renderArguments(_ value: Any?) -> String {
        switch value {
        case let text as String:
            return text
        case let object as [String: Any]:
            // Sorted keys so the same call reads the same way twice.
            let pairs = object.keys.sorted().map { key in
                "\(key): \(ErisApprovalPage.describe(object[key]))"
            }
            return pairs.joined(separator: ", ")
        case .some(let other):
            return describe(other)
        case .none:
            return ""
        }
    }

    /// A short description of one argument value, cut rather than dumped.
    private static func describe(_ value: Any?) -> String {
        switch value {
        case let text as String:
            let flat = text.replacingOccurrences(of: "\n", with: " ")
            return flat.count > 120 ? String(flat.prefix(120)) + "..." : flat
        case let number as NSNumber:
            return number.stringValue
        case let flag as Bool:
            return flag ? "true" : "false"
        case let nested as [String: Any]:
            return "{" + nested.keys.sorted().joined(separator: ", ") + "}"
        case let list as [Any]:
            return "[" + list.map { describe($0) }.joined(separator: ", ") + "]"
        case .some(let other):
            return String(describing: other)
        case .none:
            return ""
        }
    }
}

/// Read one decision answer.
extension ErisApprovalOutcome {
    /// Decode the daemon's answer to a decision.
    ///
    /// # Throws
    ///
    /// `ErisError.decoding` when the payload is not the documented shape.
    static func decode(_ data: Data) throws -> ErisApprovalOutcome {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = object["id"] as? String
        else {
            throw ErisError.decoding("the decision answer is not a JSON object with an id")
        }
        return ErisApprovalOutcome(
            id: id,
            decision: object["decision"] as? String ?? "",
            result: object["result"] as? String
        )
    }
}
