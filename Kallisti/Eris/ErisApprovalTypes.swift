import Foundation
import SwiftUI

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


// MARK: - The approvals surface

/// Approvals the daemon is holding, and the decision about each.
///
/// The transport already existed (ErisDaemonStore.clientForPairedDaemon); what did
/// not was anything that called it. A failure is reported as itself - a turn that
/// already finished, an approval that is gone, a decision the daemon refused -
/// rather than being folded into one message, because the operator needs to know
/// which of those happened.
@MainActor
final class ErisApprovalsStore: ObservableObject {
    @Published private(set) var rows: [ErisApproval] = []
    @Published private(set) var busy = false
    @Published var failure: String?

    private let store: ErisDaemonStore

    init(store: ErisDaemonStore = .shared) {
        self.store = store
    }

    /// Read the queue. Safe to call on every appearance.
    func load() async {
        guard let client = store.clientForPairedDaemon() else {
            failure = "this phone is not paired with a daemon"
            return
        }
        busy = true
        defer { busy = false }
        do {
            rows = try await client.approvals().approvals
            failure = nil
        } catch {
            failure = (error as? ErisError)?.message ?? error.localizedDescription
        }
    }

    /// Answer one. The row leaves the list only when the daemon took the decision,
    /// so a refusal stays visible with its reason instead of vanishing.
    func decide(_ approval: ErisApproval, _ decision: ErisApprovalDecision) async {
        guard let client = store.clientForPairedDaemon() else { return }
        busy = true
        defer { busy = false }
        do {
            _ = try await client.decideApproval(id: approval.id, decision: decision)
            rows.removeAll { $0.id == approval.id }
            failure = nil
        } catch {
            failure = (error as? ErisError)?.message ?? error.localizedDescription
        }
    }
}

/// The sheet: every parked command, with Allow and Deny.
struct ErisApprovalsSheet: View {
    @StateObject private var model: ErisApprovalsStore
    @Environment(\.dismiss) private var dismiss

    init(store: ErisDaemonStore = .shared) {
        _model = StateObject(wrappedValue: ErisApprovalsStore(store: store))
    }

    var body: some View {
        NavigationStack {
            List {
                if let failure = model.failure {
                    Section {
                        Text(failure)
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                }
                if model.rows.isEmpty && model.failure == nil {
                    Section {
                        Text("Nothing is waiting for a decision.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
                ForEach(model.rows) { approval in
                    Section {
                        Text(approval.tool)
                            .font(.callout.weight(.semibold))
                        Text(approval.arguments)
                            .font(.caption.monospaced())
                            .lineLimit(4)
                        if let reason = approval.reason {
                            Text(reason)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        HStack {
                            Button("Allow") {
                                Task { await model.decide(approval, .approved) }
                            }
                            .buttonStyle(.borderedProminent)
                            .disabled(model.busy)
                            Button("Deny", role: .destructive) {
                                Task { await model.decide(approval, .denied) }
                            }
                            .disabled(model.busy)
                        }
                    }
                }
            }
            .navigationTitle("Approvals")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        Task { await model.load() }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .disabled(model.busy)
                }
            }
            .task { await model.load() }
            .refreshable { await model.load() }
        }
    }
}
