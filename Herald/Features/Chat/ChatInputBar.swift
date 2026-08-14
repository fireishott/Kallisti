import Speech
import SwiftUI
import UIKit

/// Protocol for speech dictation service, allowing use without iOS 26 availability constraint.
@MainActor
protocol SpeechDictationService: AnyObject, Sendable {
    var isListening: Bool { get }
    var transcript: String { get }
    var onAutoStop: ((String) -> Void)? { get set }
    var onTranscriptChange: ((String) -> Void)? { get set }
    func startListening() async throws
    func stopListening()
}

@available(iOS 26.0, *)
extension LiveSpeechService: SpeechDictationService {}

/// Creates a speech dictation service for the current iOS version.
/// - iOS 26+: Modern DictationTranscriber / SpeechAnalyzer stack
/// - iOS 18-25: Classic SFSpeechRecognizer fallback
@MainActor
func createSpeechDictationService() -> (any SpeechDictationService)? {
    if #available(iOS 26.0, *) {
        return LiveSpeechService()
    }
    return LegacySpeechService()
}

struct ChatInputBar: View {
    @Binding var text: String
    @Binding var pendingAttachments: [PendingAttachment]
    let isStreaming: Bool
    var isFocused: FocusState<Bool>.Binding
    let onSend: () -> Void
    let onStop: () -> Void
    let onQueueNext: () -> Void   // Build 31: queue a message after the active turn
    let onAttach: () -> Void
    let onSlashCommand: (SlashCommand, String?) -> Void
    /// Build 107: image pasted into the composer. Routes through the same
    /// attachment pipeline as the picker (PendingAttachment.image) so a paste
    /// produces a staged attachment instead of a file:// URL string in the
    /// text field.
    var onPasteImage: ((UIImage) -> Void)? = nil

    @Environment(TalkStore.self) private var talkStore
    @Environment(ChatStore.self) private var chatStore
    @Environment(ProfileStore.self) private var profileStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(TabRouter.self) private var router

    @State private var speechService: (any SpeechDictationService)? = createSpeechDictationService()
    @State private var dictationBaseText = ""

    private var canSend: Bool {
        let hasText = !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let hasAttachments = !pendingAttachments.isEmpty
        let hasRunnableSlashCommand = isSlashMode && hasText && text.trimmingCharacters(in: .whitespacesAndNewlines) != "/" && !hasAttachments
        return hasRunnableSlashCommand || ((hasText || hasAttachments) && !isSlashMode)
    }

    private var isSlashMode: Bool {
        text.hasPrefix("/")
    }

    private var placeholderText: String {
        return "Reply to \(profileStore.displayProfileName)"
    }

    /// Parses the command and any trailing argument from the text field.
    private var parsedSlashInput: (command: String, argument: String?) {
        let raw = String(text.dropFirst()).lowercased()
        let parts = raw.split(separator: " ", maxSplits: 1)
        let cmd = parts.first.map(String.init) ?? raw
        let arg = parts.count > 1 ? String(parts[1]) : nil
        return (cmd, arg)
    }

    /// Uses the dynamic catalog from ChatStore (fetched from the Hermes host).
    /// Falls back to the built-in list if the catalog hasn't loaded yet.
    private var filteredCommands: [SlashCommand] {
        let query = parsedSlashInput.command.lowercased()
        let argument = parsedSlashInput.argument?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let all = chatStore.commandCatalog.filter(\.showInAutocomplete)

        if query.isEmpty {
            return all.filter { $0.suggestedArgument == nil }
        }

        if let exact = all.first(where: { $0.name == query && $0.suggestedArgument == nil }), exact.acceptsArgument {
            let argumentSuggestions = all.filter { command in
                command.name == query
                    && command.suggestedArgument != nil
                    && (argument == nil
                        || argument!.isEmpty
                        || command.suggestedArgument!.lowercased().hasPrefix(argument!))
            }
            if !argumentSuggestions.isEmpty {
                return argumentSuggestions
            }
            return [exact]
        }

        return all.filter {
            $0.suggestedArgument == nil && $0.name.hasPrefix(query)
        }
    }

    var body: some View {
        VStack(spacing: Design.Spacing.xs) {
            if isSlashMode && !filteredCommands.isEmpty {
                SlashCommandMenu(commands: filteredCommands) { command in
                    let arg = command.suggestedArgument ?? (command.acceptsArgument ? parsedSlashInput.argument : nil)
                    // The handler clears the composer only after the command is
                    // actually accepted (e.g. not refused for unreachability), so
                    // drafts survive refusals and can be retried.
                    onSlashCommand(command, arg)
                }
                .transition(.opacity.combined(with: .move(edge: .bottom)))
            }

            // Composer container
            VStack(spacing: 0) {
                // Attachment preview strip
                if !pendingAttachments.isEmpty {
                    attachmentPreviewStrip
                }

                // Text input area — Build 107: paste-intercepting UITextView
                // (image paste routes to the attachment pipeline instead of
                // inserting a file:// URL string). Return-key handling moved
                // into the coordinator; the old onKeyPress/onChange newline
                // hacks are gone.
                PasteAwareComposerTextView(
                    text: $text,
                    placeholder: speechService?.isListening == true ? "Listening..." : placeholderText,
                    isFocused: isFocused,
                    enterToSend: settingsStore.settings.enterToSend,
                    canSend: { canSend },
                    onSend: handlePrimaryAction,
                    onPasteImage: { image in
                        onPasteImage?(image)
                    }
                )
                    .accessibilityLabel(placeholderText)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.top, pendingAttachments.isEmpty ? Design.Spacing.sm : Design.Spacing.xs)
                    .padding(.bottom, Design.Spacing.xs)
                    .overlay(alignment: .topLeading) {
                        // Placeholder overlay (UITextView has no native one)
                        if text.isEmpty {
                            Text(speechService?.isListening == true ? "Listening..." : placeholderText)
                                .font(Design.Typography.body)
                                .foregroundStyle(Design.Colors.tertiaryForeground)
                                .padding(.horizontal, Design.Spacing.md)
                                .padding(.top, (pendingAttachments.isEmpty ? Design.Spacing.sm : Design.Spacing.xs) + 4)
                                .allowsHitTesting(false)
                        }
                    }

                // Bottom action bar
                HStack(spacing: Design.Spacing.xs) {
                    // + Attachment button
                    Button(action: onAttach) {
                        Image(systemName: "plus")
                            .font(.system(size: Design.Size.iconMedium, weight: .medium))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(width: 36, height: 36)
                            .background(Design.Colors.surface)
                            .clipShape(Circle())
                    }
                    .accessibilityLabel("Add attachment")

                    Spacer()

                    // Dictation mic button
                    if !isStreaming {
                        Button {
                            toggleDictation()
                        } label: {
                            Image(systemName: speechService?.isListening == true ? "stop.fill" : "mic")
                                .font(.system(size: Design.Size.iconMedium, weight: .medium))
                                .foregroundStyle(speechService?.isListening == true ? .red : Design.Colors.secondaryForeground)
                                .frame(width: 36, height: 36)
                                .background(speechService?.isListening == true ? Design.Colors.surface : .clear)
                                .clipShape(Circle())
                        }
                        .accessibilityLabel(speechService?.isListening == true ? "Stop dictation" : "Start dictation")
                    }

                    // Talk mode button (right side, before send)
                    if !isStreaming && speechService?.isListening != true && !canSend {
                        Button {
                            router.isVoiceOverlayPresented = true
                        } label: {
                            Image(systemName: "waveform")
                                .font(.system(size: Design.Size.iconMedium, weight: .medium))
                                .foregroundStyle(Design.Colors.background)
                                .frame(width: 36, height: 36)
                                .background(Design.Brand.accent)
                                .clipShape(Circle())
                        }
                        .accessibilityLabel("Start voice mode")
                        .transition(.scale.combined(with: .opacity))
                    }

                    // Send / Stop button
                    actionButton
                }
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.bottom, Design.Spacing.sm)
            }
            .background(Design.Colors.surface)
            .overlay(
                RoundedRectangle(cornerRadius: Design.CornerRadius.xxl)
                    .stroke(Design.Colors.border, lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xxl))
            .contentShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xxl))
            .onTapGesture { isFocused.wrappedValue = true }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.bottom, Design.Spacing.md)
        }
        .animation(Design.Motion.quickResponse, value: isSlashMode)
        .animation(Design.Motion.quickResponse, value: isStreaming)
        .animation(Design.Motion.quickResponse, value: canSend)
        .onAppear {
            speechService?.onTranscriptChange = { partialTranscript in
                text = mergedDictationText(partialTranscript)
            }
            speechService?.onAutoStop = { finalTranscript in
                text = mergedDictationText(finalTranscript)
                dictationBaseText = ""
            }
        }
    }

    // MARK: - Attachment Preview Strip

    private var attachmentPreviewStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Design.Spacing.sm) {
                ForEach(pendingAttachments) { attachment in
                    attachmentThumbnail(attachment)
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.top, Design.Spacing.sm)
            .padding(.bottom, Design.Spacing.xxs)
        }
    }

    private func attachmentThumbnail(_ attachment: PendingAttachment) -> some View {
        ZStack(alignment: .topTrailing) {
            Group {
                if let thumbData = attachment.thumbnailData,
                   let uiImage = UIImage(data: thumbData) {
                    Image(uiImage: uiImage)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                } else {
                    // File icon fallback
                    VStack(spacing: 4) {
                        Image(systemName: fileIcon(for: attachment.mimeType))
                            .font(.system(size: 20))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Text(attachment.fileName)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Design.Colors.surface)
                }
            }
            .frame(width: 64, height: 64)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
            .overlay(
                RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                    .stroke(Design.Colors.divider, lineWidth: 1)
            )

            // Remove button
            Button {
                withAnimation(Design.Motion.quickResponse) {
                    pendingAttachments.removeAll { $0.id == attachment.id }
                }
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 18))
                    .foregroundStyle(Design.Colors.foreground)
                    .background(Circle().fill(Design.Colors.background).padding(2))
            }
            .offset(x: 6, y: -6)
        }
    }

    private func fileIcon(for mimeType: String) -> String {
        if mimeType.hasPrefix("image/") { return "photo" }
        if mimeType == "application/pdf" { return "doc.richtext" }
        if mimeType.hasPrefix("text/") { return "doc.text" }
        return "doc"
    }

    @ViewBuilder
    private var actionButton: some View {
        if isStreaming {
            // Build 31: show both Stop and Send during active streaming.
            // Stop interrupts the current turn.  Send appears only when the
            // composer has a draft or attachment — tapping it queues the new
            // message behind the active job (Send Next) instead of silently
            // cancelling the current work.
            HStack(spacing: 8) {
                // Stop / Interrupt
                Button(action: onStop) {
                    Image(systemName: "stop.fill")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(Design.Colors.foreground)
                        .frame(width: 36, height: 36)
                        .background(Design.Colors.surface)
                        .clipShape(Circle())
                }
                .accessibilityLabel("Stop generating")

                // Send Next — only when there's a draft
                if canSend {
                    Button(action: onQueueNext) {
                        Image(systemName: "arrow.up")
                            .font(.system(size: 16, weight: .bold))
                            .foregroundStyle(Design.Colors.background)
                            .frame(width: 36, height: 36)
                            .background(Design.Brand.accent)
                            .clipShape(Circle())
                    }
                    .accessibilityLabel("Queue message after current reply")
                    .transition(.scale.combined(with: .opacity))
                }
            }
        } else if canSend {
            Button(action: handlePrimaryAction) {
                Image(systemName: "arrow.up")
                    .font(.system(size: 16, weight: .bold))
                    .foregroundStyle(Design.Colors.background)
                    .frame(width: 36, height: 36)
                    .background(Design.Brand.accent)
                    .clipShape(Circle())
            }
            .accessibilityLabel("Send message")
            .transition(.scale.combined(with: .opacity))
        }
    }

    // MARK: - Dictation

    private func toggleDictation() {
        if speechService?.isListening == true {
            speechService?.stopListening()
            text = mergedDictationText(speechService?.transcript ?? "")
            dictationBaseText = ""
        } else {
            Task {
                do {
                    dictationBaseText = text.trimmingCharacters(in: .whitespacesAndNewlines)
                    try await speechService?.startListening()
                } catch {
                    dictationBaseText = ""
                }
            }
        }
    }

    private func handlePrimaryAction() {
        if speechService?.isListening == true {
            speechService?.stopListening()
            text = mergedDictationText(speechService?.transcript ?? "")
            dictationBaseText = ""
        }
        onSend()
    }

    private func mergedDictationText(_ transcript: String) -> String {
        let trimmedTranscript = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        let base = dictationBaseText.trimmingCharacters(in: .whitespacesAndNewlines)

        if base.isEmpty { return trimmedTranscript }
        if trimmedTranscript.isEmpty { return base }
        return "\(base) \(trimmedTranscript)"
    }
}

// MARK: - Paste-Intercepting Composer Text View

/// UITextView subclass that intercepts paste.
///
/// Build 107: mirrors the Electron app's `onPasteClipboardImage` handler —
/// when the pasteboard holds an image (or a file URL that decodes to an
/// image), the paste is routed to `onPasteImage` instead of inserting the
/// `file:///...` path as plain text (the default SwiftUI TextField behavior
/// that made image paste look broken).
final class PasteInterceptingTextView: UITextView {
    var onPasteImage: ((UIImage) -> Void)?

    override func paste(_ sender: Any?) {
        let pasteboard = UIPasteboard.general

        // 1) Native image items (public.image, copied from Photos etc.)
        if pasteboard.hasImages, let image = pasteboard.image {
            onPasteImage?(image)
            return
        }

        // 2) File URL items that resolve to an image (e.g. Messages
        //    attachments copied as file:// URLs). Resolve synchronously —
        //    same cost profile as the picker's thumbnail generation.
        if let url = pasteboard.url ?? pasteboard.string.flatMap({ URL(string: $0) }),
           url.isFileURL,
           let data = try? Data(contentsOf: url),
           let image = UIImage(data: data) {
            onPasteImage?(image)
            return
        }

        super.paste(sender)
    }

    /// Auto-grow between 1 and 5 lines (matches the old
    /// `TextField(axis: .vertical).lineLimit(1...5)`).
    override var intrinsicContentSize: CGSize {
        let lineHeight = font?.lineHeight ?? 18
        let insets = textContainerInset.top + textContainerInset.bottom
        let minHeight = lineHeight + insets
        let maxHeight = lineHeight * 5 + insets
        let measured = sizeThatFits(CGSize(width: max(bounds.width, 200), height: .greatestFiniteMagnitude)).height
        return CGSize(width: UIView.noIntrinsicMetric, height: min(max(measured, minHeight), maxHeight))
    }

    /// Hardware keyboard shift+Return inserts a newline instead of sending
    /// (the old TextField onKeyPress `.ignored` path for shifted returns).
    override var keyCommands: [UIKeyCommand]? {
        let shiftReturn = UIKeyCommand(input: "\r", modifierFlags: .shift, action: #selector(insertNewlineFromKeyCommand))
        shiftReturn.wantsPriorityOverSystemBehavior = true
        return [shiftReturn]
    }

    @objc private func insertNewlineFromKeyCommand() {
        insertText("\n")
    }
}

/// Auto-growing, paste-aware composer field. Replaces the SwiftUI TextField in
/// ChatInputBar so image paste stages a PendingAttachment instead of dumping a
/// file URL string into the message text.
struct PasteAwareComposerTextView: UIViewRepresentable {
    @Binding var text: String
    let placeholder: String
    let isFocused: FocusState<Bool>.Binding
    let enterToSend: Bool
    let canSend: () -> Bool
    let onSend: () -> Void
    let onPasteImage: ((UIImage) -> Void)?

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> PasteInterceptingTextView {
        let textView = PasteInterceptingTextView()
        textView.delegate = context.coordinator
        textView.font = UIFont.systemFont(ofSize: 15)   // Design.Typography.body
        textView.textColor = UIColor(Design.Colors.foreground)
        textView.backgroundColor = .clear
        textView.textContainerInset = UIEdgeInsets(top: 4, left: 0, bottom: 4, right: 0)
        textView.textContainer.lineFragmentPadding = 0
        textView.isScrollEnabled = true
        textView.showsVerticalScrollIndicator = false
        textView.accessibilityIdentifier = "chat.composer"
        textView.onPasteImage = { image in
            context.coordinator.parent.onPasteImage?(image)
        }
        context.coordinator.updatePlaceholder(textView)
        return textView
    }

    func updateUIView(_ uiView: PasteInterceptingTextView, context: Context) {
        if uiView.text != text {
            uiView.text = text
            uiView.invalidateIntrinsicContentSize()
            uiView.isScrollEnabled = Self.needsScroll(uiView)
        }
        context.coordinator.updatePlaceholder(uiView)
        // Sync focus from the SwiftUI FocusState binding.
        if isFocused.wrappedValue && !uiView.isFirstResponder {
            uiView.becomeFirstResponder()
        } else if !isFocused.wrappedValue && uiView.isFirstResponder {
            uiView.resignFirstResponder()
        }
    }

    private static func needsScroll(_ textView: UITextView) -> Bool {
        textView.contentSize.height >= maxHeight(textView) - 1
    }

    private static func maxHeight(_ textView: UITextView) -> CGFloat {
        let lineHeight = textView.font?.lineHeight ?? 18
        let insets = textView.textContainerInset.top + textView.textContainerInset.bottom
        return lineHeight * 5 + insets   // lineLimit(1...5)
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: PasteAwareComposerTextView

        init(_ parent: PasteAwareComposerTextView) {
            self.parent = parent
        }

        func updatePlaceholder(_ textView: UITextView) {
            // UITextView has no native placeholder; ChatInputBar overlays a
            // SwiftUI Text when empty, so nothing to render here.
        }

        func textViewDidChange(_ textView: UITextView) {
            if parent.text != textView.text {
                parent.text = textView.text
            }
            textView.invalidateIntrinsicContentSize()
            textView.isScrollEnabled = PasteAwareComposerTextView.needsScroll(textView)
        }

        func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
            // Return key (hardware or virtual): send when enterToSend is on,
            // otherwise let the newline through. Handles both keyboards in one
            // place — replaces the old onKeyPress + onChange newline hack.
            if text == "\n" {
                if parent.enterToSend {
                    if parent.canSend() {
                        parent.onSend()
                    }
                    return false
                }
                return true
            }
            return true
        }

        func textViewDidBeginEditing(_ textView: UITextView) {
            parent.isFocused.wrappedValue = true
        }

        func textViewDidEndEditing(_ textView: UITextView) {
            parent.isFocused.wrappedValue = false
        }
    }
}
