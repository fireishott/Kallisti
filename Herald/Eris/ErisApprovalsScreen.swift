import Observation
import SwiftUI

// MARK: - Approvals view model

/// The queue of calls waiting on a person, as something a phone can answer.
///
/// Both routes need the `admin` scope, so the state that matters most here is a
/// refusal: a device paired from a code minted without the administration box can
/// be perfectly healthy and still have no queue. The daemon's own sentence about
/// the pairing code is shown verbatim, because a client that rewrote it would be
/// guessing at a cause it cannot see.
///
/// Deciding runs the call. The daemon executes an approved tool and answers with
/// what it said, so this model keeps the last result and shows it rather than only
/// reporting that a button was pressed.
@MainActor
@Observable
final class ErisApprovalsModel {
    /// The rows the daemon last reported, newest first.
    private(set) var approvals: [ErisApproval] = []
    /// Whether the first read is in flight.
    private(set) var isLoading = false
    /// The approval currently being decided, so one row can show its own spinner
    /// instead of freezing the list.
    private(set) var decidingID: String?
    /// The refusal that stopped the read or the decision.
    private(set) var failure: ErisFailureKind?
    /// What the last approval actually did, for the person who tapped it.
    private(set) var lastOutcome: ErisApprovalOutcome?
    /// Whether the queue has ever loaded, which is what tells an empty list apart
    /// from one that has not been read yet.
    private(set) var hasLoaded = false
    /// Whether the daemon holds more rows than this page.
    private(set) var truncated = false

    /// The transport. An actor, so nothing here blocks the main thread.
    private let client: ErisClient

    init(client: ErisClient) {
        self.client = client
    }

    /// Whether the queue is empty and known to be.
    var isEmpty: Bool {
        hasLoaded && approvals.isEmpty
    }

    // MARK: Reading

    /// Read once, the first time the lane is shown.
    func loadIfNeeded() async {
        guard !hasLoaded else { return }
        await load()
    }

    /// `GET /v1/approvals`.
    func load() async {
        guard !isLoading else { return }
        isLoading = true
        failure = nil
        defer { isLoading = false }
        do {
            let page = try await client.approvals()
            approvals = page.approvals
            truncated = page.truncated
            hasLoaded = true
        } catch {
            failure = ErisFailureKind(error)
        }
    }

    // MARK: Deciding

    /// Answer one call, then re-read the queue.
    ///
    /// The row leaves the list because the daemon settled it, not because this
    /// client removed it optimistically: a decision that failed to record must
    /// not look like a decision that landed.
    func decide(_ approval: ErisApproval, as decision: ErisApprovalDecision) async {
        guard decidingID == nil else { return }
        decidingID = approval.id
        failure = nil
        lastOutcome = nil
        defer { decidingID = nil }
        do {
            lastOutcome = try await client.decideApproval(id: approval.id, decision: decision)
            await load()
        } catch {
            failure = ErisFailureKind(error)
        }
    }

    /// Forget the last result, so the banner can be dismissed.
    func clearOutcome() {
        lastOutcome = nil
    }
}

// MARK: - Screen

/// The approvals lane: what is parked, and two buttons per row.
struct ErisApprovalsScreen: View {
    let store: ErisDaemonStore
    let model: ErisApprovalsModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Design.Spacing.md) {
                content
            }
            .padding(Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
        }
        .background(Design.Colors.background)
        .task { await model.loadIfNeeded() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await model.load() }
                } label: {
                    Label("Reload", systemImage: "arrow.clockwise")
                        .font(Design.Typography.footnote)
                }
                .disabled(model.isLoading || model.decidingID != nil)
                .accessibilityIdentifier("eris.reloadApprovals")
            }
        }
    }

    // MARK: States

    @ViewBuilder
    private var content: some View {
        if model.isLoading, !model.hasLoaded {
            ErisLoadingView(label: "Reading the approvals queue…")
        } else if let failure = model.failure, !model.hasLoaded {
            ErisFailureView(kind: failure, onRetry: { Task { await model.load() } })
        } else if model.isEmpty {
            emptyState
        } else {
            if let outcome = model.lastOutcome {
                outcomeBanner(outcome)
            }
            if let failure = model.failure {
                // A decision that failed leaves the row in place, so the failure
                // sits above the list rather than replacing it.
                ErisFailureView(kind: failure, onRetry: { Task { await model.load() } })
            }
            if model.truncated {
                Text("Showing the newest approvals. Older ones are on the daemon.")
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            ForEach(model.approvals) { approval in
                card(approval)
            }
        }
    }

    /// Nothing waiting. Says so plainly: an empty queue is a good state, not an
    /// error, so it gets neither an alarm nor a spinner.
    private var emptyState: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text("Nothing waiting")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
            Text("Calls the daemon parked for a decision show up here.")
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .accessibilityIdentifier("eris.approvalsEmpty")
    }

    /// What the last decision actually did.
    private func outcomeBanner(_ outcome: ErisApprovalOutcome) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            Text(outcome.decision == ErisApprovalDecision.approved.rawValue
                 ? "Ran it"
                 : "Denied it")
                .font(Design.Typography.footnote.weight(.semibold))
                .foregroundStyle(Design.Colors.foreground)
            if let result = outcome.result, !result.isEmpty {
                Text(result)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .lineLimit(4)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Design.Spacing.sm)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .onTapGesture { model.clearOutcome() }
        .accessibilityIdentifier("eris.approvalOutcome")
    }

    /// One parked call: what it is, why it is here, and what to do about it.
    private func card(_ approval: ErisApproval) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.xxs) {
                Image(systemName: "hand.raised")
                    .font(.system(size: Design.Size.iconSmall, weight: .semibold))
                    .foregroundStyle(Design.Colors.accent)
                Text(approval.tool)
                    .font(Design.Typography.callout.weight(.semibold))
                    .foregroundStyle(Design.Colors.foreground)
                Spacer()
                Text(approval.createdAt)
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            if !approval.arguments.isEmpty {
                Text(approval.arguments)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .lineLimit(5)
            }
            if let reason = approval.reason, !reason.isEmpty {
                Text(reason)
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            actions(approval)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .accessibilityIdentifier("eris.approval.\(approval.id)")
    }

    /// Both buttons, disabled while any decision is in flight so two taps cannot
    /// race each other into one row.
    private func actions(_ approval: ErisApproval) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Button {
                Task { await model.decide(approval, as: .approved) }
            } label: {
                if model.decidingID == approval.id {
                    ProgressView().controlSize(.small)
                } else {
                    Text(ErisApprovalDecision.approved.title)
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(model.decidingID != nil)
            .accessibilityIdentifier("eris.approve.\(approval.id)")

            Button {
                Task { await model.decide(approval, as: .denied) }
            } label: {
                Text(ErisApprovalDecision.denied.title)
            }
            .buttonStyle(.bordered)
            .disabled(model.decidingID != nil)
            .accessibilityIdentifier("eris.deny.\(approval.id)")

            Spacer()
        }
        .padding(.top, Design.Spacing.xxs)
    }
}
