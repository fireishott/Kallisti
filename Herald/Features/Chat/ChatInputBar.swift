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
    var isFocused: Binding<Bool>
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
    /// Build 127: when the host is not online (connecting / reconnecting /
    /// unreachable), the composer is read-only — no focus, no editing, no
    /// keyboard. Fixes the "connecting screen with the keyboard still up and
    /// typing" bug.
    var isEnabled: Bool = true

    @Environment(TalkStore.self) private var talkStore
    @Environment(ChatStore.self) private var chatStore
    @Environment(ProfileStore.self) private var profileStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(TabRouter.self) private var router

    @State private var speechService: (any SpeechDictationService)? = createSpeechDictationService()
    @State private var dictationBaseText = ""
    /// Build 116: set by a completed long-press so the release-tap that
    /// follows the gesture does not send/queue a second copy of the draft.
    @State private var suppressNextTap = false

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
        return "Send message"
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

            // Composer container. Build 120: iMessage-style. The text field is
            // its own capsule that stretches to fill the row; the + button
            // lives OUTSIDE the capsule on the left, the mic sits INSIDE the
            // capsule at the trailing edge, and the action button (send / stop
            // / steer / voice) sits outside on the right. This kills the old
            // one-bubble layout where the field was squeezed between the + and
            // mic icons with wide dead space on both sides.
            VStack(spacing: 0) {
                if !pendingAttachments.isEmpty {
                    attachmentPreviewStrip
                }

                HStack(alignment: .bottom, spacing: Design.Spacing.xs) {
                    // + button - outside the capsule, iMessage style.
                    // Build 128: now a plain 40pt vector (no filled
                    // background circle), matching the send arrow and
                    // waveform buttons on the right side of the row.
                    // Build 123: 40pt visual, keeps the row slim when empty.
                    Button(action: onAttach) {
                        Image(systemName: "plus")
                            .font(.system(size: 20, weight: .medium))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(width: 40, height: 40)
                    }
                    .accessibilityLabel("Add attachment")

                    // Text capsule - own background, stretches to fill
                    HStack(spacing: 0) {
                        PasteAwareComposerTextView(
                            text: $text,
                            placeholder: speechService?.isListening == true ? "Listening..." : placeholderText,
                            isFocused: isFocused,
                            enterToSend: settingsStore.settings.enterToSend,
                            canSend: { canSend },
                            onSend: handlePrimaryAction,
                            onPasteImage: { image in onPasteImage?(image) },
                            isEnabled: isEnabled
                        )
                        .accessibilityLabel(placeholderText)
                        .font(Design.Typography.body)
                        .foregroundStyle(Design.Colors.foreground)
                        // Build 127.1: hug the text content line-by-line (1-5
                        // rows). The old minHeight/idealHeight/maxHeight frame
                        // was flexible and expanded to fill the parent's proposed
                        // height, so a one-line draft jumped straight to the 5-row
                        // slab. fixedSize(vertical:) + the sizeThatFits override
                        // make the capsule track the actual text height.
                        .fixedSize(horizontal: false, vertical: true)
                        .overlay(alignment: .topLeading) {
                            if text.isEmpty {
                                Text(speechService?.isListening == true ? "Listening..." : placeholderText)
                                    .font(Design.Typography.body)
                                    .foregroundStyle(Design.Colors.tertiaryForeground)
                                    .padding(.top, 11)
                                    .allowsHitTesting(false)
                            }
                        }

                        // Mic lives INSIDE the capsule at the trailing edge.
                        // Build 123: 40pt visual keeps the empty capsule slim.
                        if !isStreaming {
                            Button { toggleDictation() } label: {
                                Image(systemName: speechService?.isListening == true ? "stop.fill" : "mic")
                                    .font(.system(size: 20, weight: .medium))
                                    .foregroundStyle(speechService?.isListening == true ? .red : Design.Colors.secondaryForeground)
                                    .frame(width: 40, height: 40)
                            }
                            .accessibilityLabel(speechService?.isListening == true ? "Stop dictation" : "Start dictation")
                        }
                    }
                    .padding(.leading, Design.Spacing.sm)
                    .background(
                        RoundedRectangle(cornerRadius: Design.CornerRadius.xl, style: .continuous)
                            .fill(Design.Colors.surface2)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: Design.CornerRadius.xl, style: .continuous)
                            .stroke(Design.Colors.border, lineWidth: 1)
                    )
                    .contentShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xl, style: .continuous))
                    .onTapGesture {
                        // Build 127: do not allow focus while the host is offline.
                        guard isEnabled else { return }
                        isFocused.wrappedValue = true
                    }

                    // Build 125: the action button SWAPS.
                    // Empty composer -> talk button (waveform accent circle)
                    // opens voice mode. Text present -> send button, a plain
                    // arrow.up vector matching the mic style - NO white/accent
                    // circle fill. While streaming, actionButton takes over
                    // (Stop / Steer).
                    if isStreaming {
                        actionButton
                    } else if speechService?.isListening != true {
                        if canSend {
                            Button(action: sendAction) {
                                Image(systemName: "arrow.up")
                                    .font(.system(size: 20, weight: .semibold))
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                    .frame(width: 40, height: 40)
                            }
                            .accessibilityLabel("Send message")
                            .accessibilityHint("Send the drafted message")
                            .simultaneousGesture(longPressToQueueGesture)
                            .transition(.scale.combined(with: .opacity))
                        } else {
                            Button {
                                router.isVoiceOverlayPresented = true
                            } label: {
                                Image(systemName: "waveform")
                                    .font(.system(size: 20, weight: .medium))
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                    .frame(width: 40, height: 40)
                                    .background(.clear)
                                    .clipShape(Circle())
                            }
                            .accessibilityLabel("Start voice mode")
                            .accessibilityHint("Open voice mode")
                            .transition(.scale.combined(with: .opacity))
                        }
                    }
                }
                .padding(.horizontal, Design.Spacing.xxs)
                .padding(.vertical, Design.Spacing.xxxs)
            }
            .padding(.bottom, Design.Spacing.xxs)
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
            // Build 116: the mid-turn send button is a steering wheel, so the
            // affordance reads as "steer the conversation" rather than a plain
            // send arrow. Long press also queues (settings-gated).
            HStack(spacing: 8) {
                // Stop / Interrupt
                Button(action: onStop) {
                    Image(systemName: "stop.fill")
                        .font(.system(size: 20, weight: .medium))
                        .foregroundStyle(.red)
                        .frame(width: 40, height: 40)
                }
                .accessibilityLabel("Stop generating")

                // Steer / Send Next — only when there's a draft
                if canSend {
                    Button(action: steerAction) {
                        Image(systemName: "steeringwheel")
                            .font(.system(size: 20, weight: .medium))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(width: 40, height: 40)
                    }
                    .accessibilityLabel("Queue message after current reply")
                    .accessibilityHint("Steer the conversation")
                    .simultaneousGesture(longPressToQueueGesture)
                    .transition(.scale.combined(with: .opacity))
                }
            }
        } else if canSend {
            // Build 121: send matches the mic style - plain vector icon,
            // no white circle. Same tap target, same secondary foreground.
            Button(action: sendAction) {
                Image(systemName: "arrow.up")
                    .font(.system(size: Design.Size.iconMedium, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .frame(width: Design.Size.minTapTarget, height: Design.Size.minTapTarget)
            }
            .accessibilityLabel("Send message")
            .simultaneousGesture(longPressToQueueGesture)
            .transition(.scale.combined(with: .opacity))
        }
    }

    /// Build 116: optional long-press on the send button queues the draft
    /// behind the active turn (steer) instead of sending immediately. Gated
    /// by the Long Press to Queue setting.
    private var longPressToQueueGesture: some Gesture {
        LongPressGesture(minimumDuration: 0.45)
            .onEnded { _ in
                guard settingsStore.settings.longPressToQueue else { return }
                suppressNextTap = true
                onQueueNext()
            }
    }

    /// Send button tap. A just-completed long press leaves `suppressNextTap`
    /// set, so the release-tap that follows must not send a second copy.
    private func sendAction() {
        guard !consumeSuppressedTap() else { return }
        handlePrimaryAction()
    }

    /// Steer button tap. Same suppression: a long press already queued the
    /// draft, so the release-tap must not queue it again.
    private func steerAction() {
        guard !consumeSuppressedTap() else { return }
        onQueueNext()
    }

    private func consumeSuppressedTap() -> Bool {
        if suppressNextTap {
            suppressNextTap = false
            return true
        }
        return false
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
    /// Build 118: when the text is empty, return EXACTLY the one-line
    /// minimum. The old code measured `sizeThatFits` on an empty view and
    /// clamped to the range, which let the frame settle at the maximumHeight
    /// when SwiftUI proposed a tall height on a fresh chat - the composer
    /// opened as a 5-row slab. Empty => one line, period.
    override var intrinsicContentSize: CGSize {
        let lineHeight = font?.lineHeight ?? 18
        let insets = textContainerInset.top + textContainerInset.bottom
        let minHeight = lineHeight + insets   // Build 121: 1-row default, period
        let maxHeight = lineHeight * 5 + insets
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return CGSize(width: UIView.noIntrinsicMetric, height: minHeight)
        }
        let measured = sizeThatFits(CGSize(width: PasteAwareComposerTextView.measurementWidth(self), height: .greatestFiniteMagnitude)).height
        return CGSize(width: UIView.noIntrinsicMetric, height: min(max(measured, minHeight), maxHeight))
    }

    /// Build 112: re-decide the scroll flag once UIKit has laid the view out
    /// with REAL bounds. The first `updateUIView` pass runs before layout
    /// (`bounds.width == 0`), and the old 200pt measurement fallback made a
    /// short message look like 5+ lines, flipping `isScrollEnabled` on. A
    /// scroll-enabled UITextView then reports `contentSize >= frame` in
    /// `sizeThatFits`, which keeps `needsScroll` true forever — the composer
    /// pinned at max height with dead space under the text. With the real
    /// width, the flag settles correctly and the box collapses to the text.
    override func layoutSubviews() {
        super.layoutSubviews()
        let shouldScroll = PasteAwareComposerTextView.needsScroll(self)
        if isScrollEnabled != shouldScroll {
            isScrollEnabled = shouldScroll
            invalidateIntrinsicContentSize()
        }
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
    // Build 121: default to ONE row. The b110+ measurement fix (real bounds,
    // not the 200pt fallback) means a 1-row empty composer no longer trips the
    // 5-row slab. Grows 1→5 as text is typed, then scrolls internally.
    static let minimumHeight: CGFloat = UIFont.systemFont(ofSize: 15).lineHeight + 8
    static let maximumHeight: CGFloat = UIFont.systemFont(ofSize: 15).lineHeight * 5 + 8

    @Binding var text: String
    let placeholder: String
    let isFocused: Binding<Bool>
    let enterToSend: Bool
    let canSend: () -> Bool
    let onSend: () -> Void
    let onPasteImage: ((UIImage) -> Void)?
    /// Build 127: read-only composer while the host is not online.
    var isEnabled: Bool = true

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> PasteInterceptingTextView {
        let textView = PasteInterceptingTextView()
        textView.delegate = context.coordinator
        textView.font = UIFont.systemFont(ofSize: 15)   // Design.Typography.body
        textView.textColor = UIColor(Design.Colors.foreground)
        textView.backgroundColor = .clear
        textView.textContainerInset = UIEdgeInsets(top: 11, left: 0, bottom: 11, right: 0)
        textView.textContainer.lineFragmentPadding = 0
        // Build 109: start NON-scrollable. A scroll-enabled UITextView is a
        // UIScrollView, so SwiftUI lets it expand to fill all proposed height
        // and the composer rendered as a huge empty slab (b107 regression).
        // needsScroll flips this to true only when content exceeds 5 lines.
        textView.isScrollEnabled = false
        textView.showsVerticalScrollIndicator = true
        textView.verticalScrollIndicatorInsets = UIEdgeInsets(top: 11, left: 0, bottom: 11, right: -4)
        textView.accessibilityIdentifier = "chat.composer"
        // Build 127: read-only while the host is offline.
        textView.isEditable = isEnabled
        textView.onPasteImage = { image in
            context.coordinator.parent.onPasteImage?(image)
        }
        context.coordinator.updatePlaceholder(textView)
        return textView
    }

    func updateUIView(_ uiView: PasteInterceptingTextView, context: Context) {
        if uiView.text != text {
            uiView.text = text
        }
        // Build 127: sync editable state. When the host drops offline, kill
        // first responder so the keyboard cannot stay up over the connecting
        // / reconnect UI, and stop accepting edits.
        if uiView.isEditable != isEnabled {
            uiView.isEditable = isEnabled
        }
        if !isEnabled && uiView.isFirstResponder {
            uiView.resignFirstResponder()
        }
        // Build 109: recompute scroll state EVERY update, not only when text
        // changes. The old guard meant an empty composer ("" == "") never
        // re-ran needsScroll, leaving isScrollEnabled = true from makeUIView
        // and the composer stretched to fill the whole chat area.
        uiView.isScrollEnabled = Self.needsScroll(uiView)
        uiView.invalidateIntrinsicContentSize()
        context.coordinator.updatePlaceholder(uiView)
        // Sync focus from the SwiftUI binding. Build 111: plain Binding, not
        // FocusState — FocusState has no .focused() anchor on a UIKit-backed
        // UITextView, so the focus engine zeroes it on every body re-render
        // (each keystroke), and the resign branch below dropped the keyboard
        // after every character. UIKit owns first responder; the binding only
        // mirrors the delegate callbacks and the tap-to-focus gesture.
        if isFocused.wrappedValue && !uiView.isFirstResponder {
            uiView.becomeFirstResponder()
        } else if !isFocused.wrappedValue && uiView.isFirstResponder {
            uiView.resignFirstResponder()
        }
    }

    /// Build 127.1: report the content-hugging height (1-5 rows) back to
    /// SwiftUI. The default UIViewRepresentable sizeThatFits returns nil, which
    /// lets the parent's proposed height drive the capsule - a one-line draft
    /// rendered as the full 5-row max. Returning the clamped intrinsic height
    /// here (paired with fixedSize(vertical:) at the call site) makes the
    /// composer grow line-by-line and cap at 5.
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: PasteInterceptingTextView, context: Context) -> CGSize? {
        let height = uiView.intrinsicContentSize.height
        let width = proposal.width ?? UIView.noIntrinsicMetric
        return CGSize(width: width, height: height)
    }

    // fileprivate: called from PasteInterceptingTextView.layoutSubviews (same file).
    fileprivate static func needsScroll(_ textView: UITextView) -> Bool {
        // Build 110: measure the TEXT, not contentSize. contentSize tracks the
        // frame SwiftUI already proposed - when the VStack proposes the full
        // chat height, contentSize balloons with it, needsScroll returns true,
        // isScrollEnabled flips back on, and the composer expands to fill the
        // screen again (the b107/b109 slab). sizeThatFits with infinite height
        // returns the height the text actually needs, independent of frame.
        let width = measurementWidth(textView)
        let measured = textView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        return measured >= maxHeight(textView) - 1
    }

    /// The width the text actually wraps at. During the first layout pass the
    /// representable's bounds are still zero, so the old hardcoded 200pt
    /// fallback measured a short message as 5+ lines and trapped the composer
    /// at max height. Use the real bounds once layout settles; before that,
    /// estimate the composer's text width (screen minus outer horizontal md
    /// padding minus the text field's own horizontal md padding).
    fileprivate static func measurementWidth(_ textView: UITextView) -> CGFloat {
        if textView.bounds.width > 0 { return textView.bounds.width }
        let screen = UIScreen.main.bounds.width
        return max(screen - Design.Spacing.md * 4, 200)
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
