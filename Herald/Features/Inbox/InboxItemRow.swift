import SwiftUI

/// Visual state for a row when the inbox is in bulk-selection mode.
/// `nil` means the row is in its normal, tap-to-open behaviour.
struct InboxItemRowSelection: Equatable {
    let id: UUID
    let isSelected: Bool
}

struct InboxItemRow: View {
    let item: InboxItem
    let onPrimaryAction: () -> Void
    let onSecondaryAction: () -> Void
    let onOpenDetails: () -> Void
    /// When non-nil the row renders in selection mode: shows a checkmark
    /// indicator, hides the Open/Dismiss buttons, and routes row taps to
    /// `onToggleSelection` instead of `onOpenDetails`. `nil` preserves the
    /// original behaviour for every other call site.
    var selectionState: InboxItemRowSelection? = nil
    var onToggleSelection: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            headerRow

            Text(item.body)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .lineLimit(3)

            if item.isActionable && !item.isRead && selectionState == nil {
                actionButtons
            }
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(selectionState?.isSelected == true ? Design.Colors.surfaceSelected : Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(
                    selectionState?.isSelected == true ? Design.Brand.accent : Design.Colors.border,
                    lineWidth: selectionState?.isSelected == true ? 1.5 : 1
                )
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
        .opacity(item.isRead ? 0.7 : 1.0)
        .contentShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
        .onTapGesture {
            if selectionState != nil {
                onToggleSelection()
            } else {
                onOpenDetails()
            }
        }
    }

    // MARK: - Header

    private var headerRow: some View {
        HStack(spacing: Design.Spacing.sm) {
            if let selectionState {
                selectionIndicator(isSelected: selectionState.isSelected)
            }

            Image(systemName: item.type.displayIcon)
                .font(.system(size: Design.Size.iconMedium))
                .foregroundStyle(item.type.displayColor)
                .frame(width: Design.Size.avatarSmall, height: Design.Size.avatarSmall)
                .background(Design.Colors.surface)
                .clipShape(Circle())

            VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
                Text(item.title)
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Colors.foreground)

                Text(item.timestamp, style: .relative)
                    .brandEyebrow(Design.Colors.tertiaryForeground)
            }

            Spacer()

            Text(item.priority.rawValue)
                .brandEyebrow()

            if !item.isRead {
                Circle()
                    .fill(item.type.displayColor)
                    .frame(width: Design.Spacing.xs, height: Design.Spacing.xs)
                    .accessibilityLabel("Unread")
            }
        }
    }

    // MARK: - Selection indicator

    private func selectionIndicator(isSelected: Bool) -> some View {
        Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
            .font(.system(size: Design.Size.iconMedium))
            .foregroundStyle(isSelected ? Design.Brand.accent : Design.Colors.tertiaryForeground)
            .accessibilityLabel(isSelected ? "Selected" : "Not selected")
    }

    // MARK: - Actions

    private var actionButtons: some View {
        HStack(spacing: Design.Spacing.sm) {
            Button {
                onPrimaryAction()
            } label: {
                Text(item.primaryAction?.title ?? defaultPrimaryActionTitle)
                    .brandEyebrow(Design.Colors.background)
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.vertical, Design.Spacing.xs)
            }
            .background(Design.Brand.accent)
            .clipShape(Capsule())
            .accessibilityLabel("\(item.primaryAction?.title ?? defaultPrimaryActionTitle) \(item.title)")

            Button {
                onSecondaryAction()
            } label: {
                Text(item.secondaryAction?.title ?? "Dismiss")
                    .brandEyebrow()
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.vertical, Design.Spacing.xs)
            }
            .background(Design.Colors.surface)
            .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
            .clipShape(Capsule())
            .accessibilityLabel("Dismiss \(item.title)")
        }
    }

    private var defaultPrimaryActionTitle: String {
        item.type == .approval ? "Approve" : "Open"
    }
}