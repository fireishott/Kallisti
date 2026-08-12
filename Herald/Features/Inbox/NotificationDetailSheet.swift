import SwiftUI

/// Full-window view of a notification-type inbox item. Opened from the
/// Inbox row's Open button when the item has no conversation reference
/// (test push, system alert). Shows the complete body, any attachments, any links from the
/// payload, the primary/secondary actions, and Dismiss + Snooze controls.
/// Build 68: replaces the previous dead-end where Open on a generic
/// notification submitted a no-op "open" action and nothing appeared.
struct NotificationDetailSheet: View {
    let item: InboxItem
    let onPrimary: () -> Void
    let onDismiss: () -> Void
    let onSnooze: (Date) -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: Design.Spacing.lg) {
                    header
                    bodyText
                    if let atts = item.attachments, !atts.isEmpty {
                        attachmentsSection(atts)
                    }
                    if let links = extractLinks, !links.isEmpty {
                        linksSection(links)
                    }
                    actionButtons
                    snoozeSection
                }
                .padding(Design.Spacing.lg)
            }
            .background(Design.Colors.background.ignoresSafeArea())
            .navigationTitle("Notification")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .font(Design.Typography.callout)
                }
            }
        }
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Design.Spacing.md) {
            Image(systemName: item.type.displayIcon)
                .font(.system(size: Design.Size.iconLarge))
                .foregroundStyle(item.type.displayColor)
                .frame(width: Design.Size.avatarMedium, height: Design.Size.avatarMedium)
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
        }
    }

    // MARK: - Body

    private var bodyText: some View {
        Text(item.body)
            .font(Design.Typography.body)
            .foregroundStyle(Design.Colors.secondaryForeground)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Attachments

    private func attachmentsSection(_ atts: [MessageAttachment]) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Text("ATTACHMENTS")
                .brandEyebrow(Design.Colors.tertiaryForeground)
            MessageAttachmentsView(attachments: atts, alignment: .leading)
        }
    }

    // MARK: - Links

    private var extractLinks: [URL]? {
        guard let payload = item.payload else { return nil }
        let urlStrings = [payload["url"], payload["link"], payload["deepLink"]]
            .compactMap { $0 }
            .filter { !$0.isEmpty }
        let urls = urlStrings.compactMap { URL(string: $0) }
        return urls.isEmpty ? nil : urls
    }

    private func linksSection(_ links: [URL]) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Text("LINKS")
                .brandEyebrow(Design.Colors.tertiaryForeground)
            ForEach(links, id: \.self) { url in
                Link(destination: url) {
                    HStack {
                        Image(systemName: "arrow.up.right.square")
                            .font(.system(size: Design.Size.iconSmall))
                        Text(url.absoluteString)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer()
                    }
                    .brandEyebrow()
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.vertical, Design.Spacing.sm)
                    .background(Design.Colors.surface)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                }
                .buttonStyle(.plain)
            }
        }
    }

    // MARK: - Actions

    private var actionButtons: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            if item.primaryAction != nil || item.type == .approval {
                Button(action: {
                    onPrimary()
                    dismiss()
                }) {
                    Text(item.primaryAction?.title ?? "Approve")
                        .brandEyebrow(Design.Colors.background)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, Design.Spacing.sm)
                }
                .buttonStyle(.plain)
                .background(Design.Brand.accent)
                .clipShape(Capsule())
            }

            Button(role: .destructive, action: {
                onDismiss()
                dismiss()
            }) {
                Text(item.secondaryAction?.title ?? "Dismiss")
                    .brandEyebrow()
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, Design.Spacing.sm)
            }
            .buttonStyle(.plain)
            .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
            .clipShape(Capsule())
        }
    }

    // MARK: - Snooze

    private var snoozeSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Text("SNOOZE")
                .brandEyebrow(Design.Colors.tertiaryForeground)
            HStack(spacing: Design.Spacing.sm) {
                ForEach(InboxStore.snoozeOptions, id: \.title) { option in
                    Button {
                        onSnooze(Date().addingTimeInterval(option.interval))
                        dismiss()
                    } label: {
                        Text(option.title)
                            .brandEyebrow()
                            .padding(.horizontal, Design.Spacing.md)
                            .padding(.vertical, Design.Spacing.xs)
                    }
                    .background(Design.Colors.surface)
                    .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
                    .clipShape(Capsule())
                }
            }
        }
    }
}
