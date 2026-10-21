import SwiftUI

// MARK: - Failure classification
//
// Every screen on the paired surface can fail the same four ways, and each one
// needs a different thing from the person reading it:
//
//   * 401  - the refresh already ran inside the client and still came back
//            unauthorized, so the refresh token is dead and only a new pairing
//            fixes it. A retry button here would be a lie.
//   * 403  - the token is real and the daemon said no. For `/v1/settings` this is
//            always `scope_denied`, because the device was paired from a code
//            minted without the administration box. The daemon's own sentence is
//            shown verbatim: it names the fix, and a client cannot improve on it.
//   * transport - the daemon did not answer at all. The credential may still be
//            fine, so this is the one failure where "Retry" is the right offer.
//   * anything else - the daemon answered and the answer was not what the
//            contract promises. The typed description is shown as-is.

/// What one failed Eris call means for the person looking at it.
enum ErisFailureKind: Equatable, Sendable {
    /// The pair is no longer accepted and a refresh did not fix it.
    case unauthorized(message: String, code: String?)
    /// The token carries no `admin` scope, so settings and approvals are closed.
    case adminScopeDenied(message: String)
    /// Another scope refusal.
    case forbidden(message: String, code: String?)
    /// The request never produced an HTTP answer.
    case transport(message: String)
    /// A server answer that is not one of the above.
    case other(message: String, code: String?)

    /// Classify one thrown error.
    init(_ error: any Error) {
        guard let eris = error as? ErisError else {
            self = .other(message: error.localizedDescription, code: nil)
            return
        }
        if eris.isAdminScopeDenied {
            self = .adminScopeDenied(
                message: eris.message ?? "the daemon refused the request for want of the admin scope"
            )
        } else if eris.isUnauthorized {
            self = .unauthorized(
                message: eris.message ?? "the daemon did not accept the pairing",
                code: eris.code
            )
        } else if eris.isForbidden {
            self = .forbidden(
                message: eris.message ?? "the daemon refused the request",
                code: eris.code
            )
        } else if eris.isTransport {
            self = .transport(message: eris.message ?? "the daemon did not answer")
        } else {
            self = .other(
                message: eris.errorDescription ?? error.localizedDescription,
                code: eris.code
            )
        }
    }

    /// The one-line heading.
    var headline: String {
        switch self {
        case .unauthorized:
            return "This pairing expired"
        case .adminScopeDenied:
            return "Paired without administration"
        case .forbidden:
            return "Not allowed"
        case .transport:
            return "Can't reach the daemon"
        case .other:
            return "Eris failed"
        }
    }

    /// The detail under the heading. For the two scope cases and the 401 this is
    /// the daemon's own message, not a paraphrase.
    var detail: String {
        switch self {
        case .unauthorized(let message, _):
            return message
        case .adminScopeDenied(let message):
            return message
        case .forbidden(let message, _):
            return message
        case .transport(let message):
            return message
        case .other(let message, _):
            return message
        }
    }

    /// The stable machine code, when the failure carried one.
    var code: String? {
        switch self {
        case .unauthorized(_, let code), .forbidden(_, let code), .other(_, let code):
            return code
        case .adminScopeDenied, .transport:
            return nil
        }
    }

    /// Whether trying the same call again could plausibly work.
    ///
    /// False for a dead pair (only re-pairing helps) and false for a scope
    /// refusal (the token will never carry the scope it lacks).
    var canRetry: Bool {
        switch self {
        case .unauthorized, .adminScopeDenied, .forbidden:
            return false
        case .transport, .other:
            return true
        }
    }

    /// Whether the fix is pairing this device again.
    var needsPairing: Bool {
        switch self {
        case .unauthorized, .adminScopeDenied:
            return true
        case .forbidden, .transport, .other:
            return false
        }
    }
}

// MARK: - Failure view

/// The one box every paired screen shows when a call fails.
///
/// It never dead-ends: a failure that a retry can fix gets a retry, a failure
/// that needs a new pairing gets the pairing action, and the daemon's own message
/// is always on screen so nobody has to guess which of the two happened.
struct ErisFailureView: View {
    let kind: ErisFailureKind
    /// Offered only when another attempt could plausibly work.
    var onRetry: (() -> Void)?
    /// Offered for a dead pair: dropping it lands back on the pairing form.
    var onPairAgain: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.xxs) {
                Image(systemName: icon)
                    .font(.system(size: Design.Size.iconSmall, weight: .semibold))
                Text(kind.headline)
                    .font(Design.Typography.headline)
                Spacer(minLength: 0)
            }
            .foregroundStyle(kind.canRetry ? Design.Colors.warning : Design.Colors.danger)

            Text(kind.detail)
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .fixedSize(horizontal: false, vertical: true)

            if let code = kind.code, !code.isEmpty {
                Text("code: \(code)")
                    .font(Design.Typography.caption2)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }

            actions
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .accessibilityIdentifier("eris.failure")
    }

    private var icon: String {
        switch kind {
        case .unauthorized: return "key.slash"
        case .adminScopeDenied: return "lock.slash"
        case .forbidden: return "hand.raised"
        case .transport: return "antenna.radiowaves.left.and.right.slash"
        case .other: return "exclamationmark.triangle"
        }
    }

    @ViewBuilder
    private var actions: some View {
        HStack(spacing: Design.Spacing.xs) {
            if kind.canRetry, let onRetry {
                Button(action: onRetry) {
                    Text("Retry")
                        .font(Design.Typography.footnote)
                        .foregroundStyle(Design.Colors.background)
                        .padding(.horizontal, Design.Spacing.md)
                        .frame(minHeight: Design.Size.minTapTarget)
                        .background(Design.Brand.accent)
                        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.pill))
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("eris.retry")
            }
            if kind.needsPairing, let onPairAgain {
                Button(action: onPairAgain) {
                    Text("Pair again")
                        .font(Design.Typography.footnote)
                        .foregroundStyle(Design.Colors.danger)
                        .padding(.horizontal, Design.Spacing.md)
                        .frame(minHeight: Design.Size.minTapTarget)
                        .background(Design.Colors.surface2)
                        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.pill))
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("eris.pairAgain")
            }
        }
        .padding(.top, Design.Spacing.xxs)
    }
}

// MARK: - Loading view

/// The one spinner every paired screen uses while its first page loads.
struct ErisLoadingView: View {
    var label: String = "Loading…"

    var body: some View {
        HStack(spacing: Design.Spacing.sm) {
            ProgressView().tint(Design.Brand.accent)
            Text(label)
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
            Spacer(minLength: 0)
        }
        .padding(Design.Spacing.md)
        .accessibilityIdentifier("eris.loading")
    }
}
