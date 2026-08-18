import SwiftUI
import UIKit

/// Inline marquee for the compact model pill. Auto-scrolls the full model
/// name horizontally when it overflows its container, and falls back to a
/// static (truncated) label when Reduce Motion is enabled or the text fits.
///
/// Inlined into ChatScreen.swift because only the model pill uses it; creating
/// a new file would require registering it in the .pbxproj and that churn is
/// not warranted for a single call site.
private struct MarqueeText<StaticFallback: View>: View {
    let text: String
    /// Truncated/static representation used under Reduce Motion. Lets the
    /// caller preserve its existing accessibility/layout conventions when
    /// animation is suppressed.
    let fallback: () -> StaticFallback

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        if reduceMotion {
            fallback()
        } else {
            MarqueeText.Animated(text: text)
        }
    }
}

private extension MarqueeText {
    /// The animated marquee itself. Separated from the wrapper so the
    /// Reduce-Motion branch never instantiates any timing state.
    struct Animated: View {
        let text: String

        // Measured layout of one copy of the text. We render two copies
        // (text + separator gap + text) and translate the whole row by
        // -singleWidth to loop seamlessly without a visible reset.
        @State private var singleWidth: CGFloat = 0
        @State private var containerWidth: CGFloat = 0
        @State private var phase: CGFloat = 0
        @State private var animationTask: Task<Void, Never>?

        /// Gap between the two copies. Larger than typical kerning so the
        /// loop wrap point isn't a visual seam.
        private let gap: CGFloat = 32
        /// Scroll speed in points-per-second. Tuned to be readable but not
        /// glacial — roughly 30pt/s.
        private let pointsPerSecond: CGFloat = 30

        var body: some View {
            // onGeometryChange (iOS 18+) replaces the older
            // Color.clear + PreferenceKey dance — no risk of layout
            // feedback loops and no extra transparent views in the tree.
            GeometryReader { _ in
                content
            }
            .frame(height: measuredHeight)
            .onGeometryChange(for: CGFloat.self) { geo in
                geo.size.width
            } action: { _, newWidth in
                containerWidth = newWidth
            }
            // onAppear alone isn't enough: when the first frame
            // renders singleLabel (both widths are still 0), the view
            // re-renders into scrollingRow without firing onAppear
            // again. Drive animation off shouldAnimate so we start as
            // soon as overflow is detected.
            .onChange(of: shouldAnimate, initial: true) { _, isOverflowing in
                if isOverflowing {
                    startAnimation()
                } else {
                    animationTask?.cancel()
                    phase = 0
                }
            }
            .onDisappear { animationTask?.cancel() }
            .onChange(of: text) { _, _ in
                // Restart the loop when the model changes so phase
                // begins from the natural left-aligned position.
                animationTask?.cancel()
                phase = 0
                if shouldAnimate { startAnimation() }
            }
        }

        @ViewBuilder
        private var content: some View {
            if shouldAnimate {
                scrollingRow
            } else {
                // Text fits: render a single static label so
                // accessibility (VoiceOver) reads the full string and
                // the visual is identical to a plain Text.
                singleLabel
            }
        }

        // MARK: - Subviews

        /// Two copies side-by-side, offset so the second copy fills the
        /// visible region while the first scrolls off the leading edge.
        /// When phase reaches -singleWidth, resetting to 0 is invisible
        /// because the second copy is now in the first copy's starting
        /// position — the loop appears seamless.
        private var scrollingRow: some View {
            HStack(spacing: gap) {
                singleLabel
                singleLabel
            }
            .offset(x: phase)
            // Clip horizontally so the off-screen copy doesn't bleed into
            // adjacent toolbar items.
            .clipped()
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(text))
        }

        /// A single copy of the text with the same font/colour treatment the
        /// pill used before. Uses onGeometryChange (iOS 18+) to capture
        /// its intrinsic width without a nested GeometryReader.
        private var singleLabel: some View {
            Text(text)
                .font(.system(size: 12, weight: .medium, design: .monospaced))
                .foregroundStyle(Design.Colors.foreground)
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
                .onGeometryChange(for: CGFloat.self) { geo in
                    geo.size.width
                } action: { _, newWidth in
                    singleWidth = newWidth
                }
        }

        // MARK: - Animation logic

        private var shouldAnimate: Bool {
            // Only animate once both measurements have a non-zero value
            // (avoids a one-frame snap on first appearance) AND the text
            // genuinely overflows the available container.
            singleWidth > 0 && containerWidth > 0 && singleWidth > containerWidth
        }

        private var measuredHeight: CGFloat {
            // The label is .system(size:12), roughly 16pt line height.
            // Using a small fixed height keeps the pill from collapsing
            // before the first layout pass completes.
            16
        }

        private func startAnimation() {
            // Guard: width may still be 0 on the very first onAppear.
            guard singleWidth > 0, containerWidth > 0,
                  singleWidth > containerWidth else { return }
            // Duration scales with how far we need to scroll. A longer
            // model name takes proportionally longer to traverse the
            // viewport, preserving a constant reading speed.
            let distance = singleWidth + gap
            let duration = max(2.0, Double(distance / pointsPerSecond))

            // Reset to a known phase before kicking off the loop so a
            // restart (model change, view re-entry) doesn't inherit a
            // mid-animation offset.
            phase = 0
            animationTask?.cancel()
            animationTask = Task { @MainActor in
                while !Task.isCancelled {
                    // Use a linear curve for a smooth, predictable marquee —
                    // no easing into/out of each loop, which would feel stuttery.
                    withAnimation(.linear(duration: duration)) {
                        phase = -singleWidth
                    }
                    try? await Task.sleep(nanoseconds: UInt64(duration * 1_000_000_000))
                    if Task.isCancelled { return }
                    // Snap back to 0 instantly (no animation) so the loop
                    // wrap is invisible: the trailing copy has just taken
                    // the leading copy's position.
                    phase = 0
                }
            }
        }
    }
}

struct ChatScreen: View {
    @Environment(ChatStore.self) private var chatStore
    @Environment(ModelStore.self) private var modelStore
    @Environment(ProfileStore.self) private var profileStore
    @Environment(KallistiHostStore.self) private var hostStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(ThemeManager.self) private var themeManager
    @Environment(TabRouter.self) private var router
    @Environment(HeraldCanvasStore.self) private var canvasStore
    @Environment(SessionListStore.self) private var sessionListStore
    @Environment(AppContainer.self) private var container
    @Binding var isSessionDrawerOpen: Bool

    @State private var pendingAttachments: [PendingAttachment] = []

    /// Backed by ChatStore so the draft survives view recreation during reconnects.
    private var messageTextBinding: Binding<String> {
        Binding(
            get: { chatStore.loadDraft(for: chatStore.conversation?.id ?? UUID()) },
            set: { chatStore.saveDraft($0, for: chatStore.conversation?.id ?? UUID()) }
        )
    }
    @State private var showClearConfirmation = false
    @State private var showStatusCard = false
    @State private var scrollProxy: ScrollViewProxy?
    // Build 111: plain @State, not @FocusState. The composer is a UIKit-backed
    // UITextView (no .focused() anchor), so FocusState gets zeroed by the focus
    // engine on every re-render and the keyboard dropped after each keystroke.
    @State private var isComposerFocused = false
    // Build 128.1: visible catch-up button spinner state.
    @State private var isCatchingUp = false

    // Scroll debounce during streaming — coalesces multiple scrollToBottom()
    // triggers from different onChange handlers into one per ~100ms, preventing
    // the "chats fly off screen" effect when delta flushes and stream-end both
    // fire scroll requests in the same run loop.
    @State private var autoScrollTask: Task<Void, Never>?
    @State private var isUserScrolling = false
    @State private var userScrollTimer: Timer?
    @State private var lastKnownContentLength: Int = 0
    private static let scrollDebounceInterval: Duration = .milliseconds(100)

    @State private var showAttachmentPicker = false
    @State private var showCanvas = false
    /// Build 128.52: queue manager sheet - view/edit/delete queued messages.
    @State private var showQueueManager = false
    /// Build 118: breathing animation state for the chat-switch overlay coin.
    @State private var switchCoinBreathes = false

    // Scroll-to-bottom arrow: only show when the user has actually scrolled
    // away from the bottom, not on every drag gesture. Uses onScrollGeometryChange
    // (iOS 18+) to detect proximity to the content bottom.
    @State private var isNearBottom = true
    // Build 31: show Jump to Latest whenever the user is scrolled away from
    // the bottom, independently of the old 3s drag timer window.  The previous
    // condition (isUserScrolling && !isNearBottom) hid the arrow after the
    // timer expired even though the user was still reading history.
    private var showScrollArrow: Bool {
        !isNearBottom
    }

    var body: some View {
        scrollAnchored {
            ZStack {
            if settingsStore.settings.chatDisplayMode == .rich {
                ChatWallpaperBackground(
                    wallpaper: settingsStore.settings.chatWallpaper,
                    tint: themeManager.preset.accent
                )
                .ignoresSafeArea()

                // Scrim: Herald messages render as plain text with no bubble background,
                // and user bubbles use a near-transparent surface tint (Design.Colors.surface),
                // so busy wallpapers (gradients/textures/photos) need dimming here to keep
                // text legible. `.default` is already a near-flat system background, so it's
                // left unscrimmed.
                if wallpaperScrimOpacity > 0 {
                    Design.Colors.background
                        .opacity(wallpaperScrimOpacity)
                        .ignoresSafeArea()
                }
            }

            VStack(spacing: 0) {
                if pairingStore.isPaired, hostStore.connectionState != .online {
                    connectionBanner
                }
                // D4: contextWarningBanner removed — the percentage was
                // fabricated from cumulative billing tokens / 256K fallback.
                // The context ring in the model pill already shows real data.
                // D3: Belt-and-braces — require an actual in-flight stream
                // before showing the banner, so a latched .reconnecting can't
                // survive even if the store fix is bypassed.
                // Build 64: also show whenever the store has a live stall
                // snapshot, so the banner appears the moment a stall is
                // declared (the snapshot is set in `markStalled` from the
                // same call sites that flip streamingPhase). Inside the
                // view, the TimelineView body returns an empty view when
                // no snapshot is present, so the outer `if` is purely
                // for layout cost avoidance.
                if chatStore.isStreaming, chatStore.stallSnapshot != nil || chatStore.streamingPhase == .stalled || chatStore.streamingPhase == .reconnecting {
                    streamingPhaseBanner
                }
                // Build 33: a gateway restart supersedes transport banners —
                // the stream was suspended on purpose, so explain why.
                if chatStore.restartInProgress {
                    restartBanner
                }
                if settingsStore.settings.chatDisplayMode == .terminal {
                    // Build 128.82: REAL embedded terminal. The connector's
                    // /v1/terminal WS bridge spawns `hermes --tui` in a PTY
                    // on the host; SwiftTerm renders the actual CLI. Input,
                    // resize, ESC all ride the socket. No themed skin.
                    TUITerminalScreen()
                } else {
                    messageList
                }
                // Build 128.50: queue status bar - shows held/queued state and
                // a Hold/Release toggle so queued messages don't silently fire
                // after the active turn (Electron parity).
                if chatStore.queuedCountForCurrentConversation > 0 || chatStore.isQueueHeld {
                    queueStatusBar
                }
                // Build 128.78: in TUI mode the terminal view owns input - the iOS
                // composer is hidden entirely.
                if settingsStore.settings.chatDisplayMode == .rich {
                    ChatInputBar(
                    text: messageTextBinding,
                    pendingAttachments: $pendingAttachments,
                    isStreaming: chatStore.isStreaming || chatStore.isServerTurnActive,
                    isFocused: $isComposerFocused,
                    onSend: sendMessage,
                    onStop: { chatStore.cancelStreaming() },
                    onQueueNext: {
                        // Build 31: enqueue a message to send after the active
                        // turn finishes.  Freeze the draft now so later edits
                        // don't change the queued text.
                        // Build 33 WSB: the queue is durable — the outbox owns
                        // the text before the composer is cleared, and the
                        // submit phase runs immediately (it no-ops while a job
                        // is active and the FIFO chain picks the item up after).
                        let frozenText = chatStore.loadDraft(for: chatStore.conversation?.id ?? UUID())
                        let frozenAttachments = pendingAttachments
                        let conversationID = chatStore.conversation?.id
                        Task {
                            guard chatStore.queueNextMessage(text: frozenText, attachments: frozenAttachments) != nil else { return }
                            if let cid = conversationID { chatStore.clearDraft(for: cid) }
                            pendingAttachments = []
                            await chatStore.submitNextEligible(for: conversationID)
                        }
                    },
                    onAttach: { showAttachmentPicker = true },
                    onSlashCommand: handleSlashCommand,
                    onPasteImage: { image in
                        handleAttachmentResult(.image(image))
                    },
                    // Build 127: composer is read-only while the host is not
                    // online (connecting / reconnecting / unreachable). Kills
                    // the "keyboard still up and typing on the connecting
                    // screen" bug.
                    // Build 128.73: gate on the LIVE socket status, not the
                    // hostStore probe. hostInfo() can fail or go stale on its
                    // own (connector restart, refresh gap) and flip
                    // connectionState offline while the WS is genuinely up -
                    // the loading screen dismissed but the composer stayed
                    // locked, so the keyboard never came up. .connected /
                    // .degraded = socket is alive; everything else read-only.
                    isEnabled: chatStore.connectionStatus == .connected
                        || chatStore.connectionStatus == .degraded
                )
                } // end rich-mode ChatInputBar gate (Build 128.78)

                // Build 128.76: clarify card. When the agent parks the turn
                // on a clarify question, show an answerable card above the
                // composer instead of letting the turn hang for the full
                // clarify_timeout. Rendered in BOTH rich and terminal modes.
                if let pending = chatStore.pendingClarify {
                    ClarifyCardView(
                        clarify: pending,
                        onSubmit: { answer in
                            chatStore.submitClarifyAnswer(answer)
                        },
                        onDismiss: {
                            chatStore.pendingClarify = nil
                        }
                    )
                }

                // Build 118: frosted chat-switch overlay. Shown while the
                // target conversation loads from the host so switching chats
                // never looks like a frozen old thread.
                if chatStore.isSwitchingConversation {
                    chatSwitchOverlay
                }
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbarContent }
        .toolbarBackground(.hidden, for: .navigationBar)
        }


        .confirmationDialog(
            "Clear Conversation",
            isPresented: $showClearConfirmation,
            titleVisibility: .visible
        ) {
            Button("Clear", role: .destructive) {
                Task { await performClear() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This will archive the current conversation and start a new session. This cannot be undone.")
        }
        // Build 31: adaptive attachment picker — the sheet owns its detents
        // (.medium, .large) and drag indicator internally via the NavigationStack.
        .sheet(isPresented: $showAttachmentPicker) {
            AttachmentPickerSheet { result in
                handleAttachmentResult(result)
            }
        }
        .sheet(isPresented: $showHeraldHub) {
            HeraldSelectorSheet(initialTab: heraldHubInitialTab)
                .environment(modelStore)
                .environment(chatStore)
                .environment(profileStore)
                .environment(hostStore)
                .presentationDetents([.large, .medium])
                .presentationDragIndicator(.visible)
        }
        .sheet(isPresented: $showQueueManager) {
            QueueManagerSheet(
                queued: chatStore.queuedMessagesForCurrentConversation,
                onEdit: { id, newText in chatStore.editQueuedMessage(id, newText: newText) },
                onDelete: { id in chatStore.removeQueuedItem(id) }
            )
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
        }
        .sheet(isPresented: $showCanvas) {
            CanvasView(store: canvasStore, onDismiss: { showCanvas = false })
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
                // Build 78: pipe live tool activities from the current
                // streaming message into the canvas store so the Live tab
                // updates in real time. Clear them when the canvas dismisses.
                .onAppear {
                    let streamingID = chatStore.streamingMessageID
                    if let sID = streamingID,
                       let msg = chatStore.conversation?.messages.first(where: { $0.id == sID }) {
                        canvasStore.liveToolActivities = msg.toolActivities
                    } else {
                        canvasStore.liveToolActivities = []
                    }
                }
                .onDisappear {
                    canvasStore.liveToolActivities = []
                    canvasStore.activeTab = .artifact
                }
        }
        // Build 78: keep canvasStore.liveToolActivities in sync with the
        // streaming message's tool activity list. When the canvas is
        // presented, changes to tool activities push through here so
        // the Live tab list stays live.
        .onChange(of: chatStore.streamingMessageID) { _, sid in
            guard showCanvas else { return }
            if let sid = sid,
               let msg = chatStore.conversation?.messages.first(where: { $0.id == sid }) {
                canvasStore.liveToolActivities = msg.toolActivities
            } else {
                canvasStore.liveToolActivities = []
            }
        }
        .onChange(of: chatStore.conversation?.messages.count) { _, _ in
            guard showCanvas,
                  let sid = chatStore.streamingMessageID,
                  let msg = chatStore.conversation?.messages.first(where: { $0.id == sid }) else { return }
            // Cheap re-pull on message updates; onChange(of: toolActivities)
            // would be more targeted but Message is a value type whose
            // mutations don't propagate as array-element changes.
            canvasStore.liveToolActivities = msg.toolActivities
        }
    }

    /// Scroll anchoring: load/poll lifecycle plus scroll-ownership onChange handlers,
    /// extracted from `body` so the SwiftUI type-checker stays under its
    /// complexity ceiling. Build 103 added a conversation-identity onChange that
    /// pushed the monolithic body expression past "unable to type-check in
    /// reasonable time" — moving the whole anchor group here splits the expression.
    private func scrollAnchored<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .task {
                chatStore.setPollingEnabled(true)
                async let hostRefresh: Void = hostStore.refresh()
                async let conversationLoad: Void = chatStore.loadConversationIfNeeded()
                // Build 124: when the thread is already cached (typical on
                // appear), loadConversationIfNeeded no-ops and the user sees
                // whatever the last fetch returned — stale if another device
                // advanced the thread while this view was not on screen.
                // One quiet refresh catches the thread up and surfaces the
                // "N new messages" pill when rows landed that this device
                // never rendered.
                async let catchUp: Void = chatStore.performForegroundCatchUp()
                // The active profile belongs to the connector, not the local chat
                // session cache. Refresh it whenever Chat becomes active so a
                // stale pre-pairing value such as ".hermes" is never retained in
                // the composer after the host reconnects or changes profile.
                async let profileLoad: Void = profileStore.loadProfiles(force: true)
                async let modelLoad: Void = modelStore.loadModels()
                // Build 33 WSB: reconcile the durable outbox - settle accepted
                // jobs against the relay, resubmit items whose backoff elapsed,
                // and submit queued items for this conversation.
                async let outboxRecovery: Void = chatStore.recoverOutbox()
                await conversationLoad
                // Scroll to most recent messages after loading.
                // Build 103: force so a stale isUserScrolling flag from a prior
                // conversation cannot suppress landing on the latest message.
                try? await Task.sleep(for: .milliseconds(150))
                isUserScrolling = false
                scrollToBottom(animate: false, force: true)
                _ = await (hostRefresh, profileLoad, modelLoad, outboxRecovery, catchUp)
            }
            .task {
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(10))
                    guard !Task.isCancelled else { break }
                    await hostStore.refresh()
                }
            }
            .onDisappear {
                chatStore.setPollingEnabled(false)
            }
            .onChange(of: chatStore.conversation?.id.uuidString ?? "") { _, _ in
                // Build 103: entering a different thread must clear scroll ownership
                // and land on the latest message, not inherit a stale "user scrolled
                // up" flag from the previous conversation.
                isUserScrolling = false
                isNearBottom = true
                scrollToBottom(animate: false, force: true)
            }
            .onChange(of: chatStore.conversation?.messages.count ?? 0) {
                guard chatStore.streamingMessageID == nil else { return }
                scrollToBottom()
            }
            .onChange(of: streamingContentLength) {
                // Keep the view anchored to the bottom as streaming content grows.
                // Without this, the ScrollView drifts — the message count is stable
                // during streaming so the count observer above skips.
                // Uses the same throttled scrollToBottom path (not a raw scrollTo)
                // so the 500ms throttle and user-scroll deferral apply.
                guard chatStore.streamingMessageID != nil else { return }
                lastKnownContentLength = streamingContentLength
                scrollToBottom()
            }
            // Build 31: the 300ms polling timer was removed.  The content-length
            // onChange observer above is sufficient for text-streaming scroll; late
            // tool cards, image layout, and thought disclosure invalidate geometry
            // naturally through SwiftUI's layout cycle, and the debounced
            // scrollToBottom coalesces them into one post-layout scroll.
            .onChange(of: chatStore.pendingMessageSentAt) {
                // User sent a message — resume auto-scroll.
                // Removed the guard !isComposerFocused: this suppressed the
                // scroll when the keyboard was up during send, causing the new
                // user bubble to render below the thinking placeholder.
                isUserScrolling = false
                Task { @MainActor in
                    try? await Task.sleep(for: .milliseconds(100))
                    scrollToBottom(animate: true, force: true)
                }
            }
            .onChange(of: chatStore.streamingMessageID) { old, new in
                if old == nil && new != nil {
                    // Streaming just STARTED (thinking bubble appears below
                    // the user's message). Bring it into view immediately -
                    // the placeholder is inserted before any content grows,
                    // so streamingContentLength alone may not fire a scroll
                    // until the first delta lands.
                    isUserScrolling = false
                    Task { @MainActor in
                        try? await Task.sleep(for: .milliseconds(80))
                        scrollToBottom(animate: true, force: true)
                    }
                } else if old != nil && new == nil {
                    // Streaming just ended. Scroll to the last stable (non-streaming)
                    // message to avoid the mutable streamingCompositeID race that
                    // caused thinking dots to render above the next sent reply.
                    isUserScrolling = false
                    userScrollTimer?.invalidate()
                    Task { @MainActor in
                        try? await Task.sleep(for: .milliseconds(150))
                        let stableTarget = chatStore.conversation?.messages
                            .last(where: { !$0.isStreaming })?.id
                            ?? chatStore.conversation?.messages.last?.id
                        if let lastID = stableTarget {
                            scrollProxy?.scrollTo(lastID, anchor: .bottom)
                        }
                    }
                }
            }
    }

    // MARK: - Wallpaper

    /// Dimming applied between the wallpaper and the chat content for legibility.
    /// `.default` renders as a near-flat system background already, so it's left
    /// unscrimmed; every other style (gradients, textures, solid tint, custom photo)
    /// gets a theme-aware scrim since message content has little to no opaque backing.
    private var wallpaperScrimOpacity: Double {
        switch settingsStore.settings.chatWallpaper {
        case .default:
            0
        case .custom:
            0.65
        default:
            0.35
        }
    }

    // MARK: - Toolbar

    @ToolbarContentBuilder
    private var toolbarContent: some ToolbarContent {
        if DeviceClass.isPhone {
            // iPhone: always uses the compact phone toolbar
            iPhoneToolbarContent
        } else {
            // iPad/Mac: width-adaptive — picks wide or compact based on
            // the chat column's available width, not the device idiom.
            adaptiveToolbarContent
        }
    }

    // iPhone: hamburger on leading; bounded status chip as principal; Canvas on trailing
    @ToolbarContentBuilder
    private var iPhoneToolbarContent: some ToolbarContent {
        // Build 128.81: no session drawer button in TUI mode - the terminal
        // owns the whole surface, and there is no left session bar.
        if settingsStore.settings.chatDisplayMode == .rich {
            ToolbarItem(placement: .topBarLeading) {
                Button {
                    withAnimation(Design.Motion.standard) {
                        isSessionDrawerOpen.toggle()
                    }
                } label: {
                    Image(systemName: "line.3.horizontal")
                        .font(.system(size: 16, weight: .medium))
                        .foregroundStyle(Design.Colors.foreground)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Open session drawer")
            }
        }
        // Build 128.88: in TUI mode the terminal owns the whole surface -
        // including the toolbar center. TUITerminalScreen supplies its own
        // .principal (statusText), so adding compactStatusControl here too
        // would put two principals on one compact toolbar and squeeze the
        // terminal. Keep the chip for rich mode only.
        if settingsStore.settings.chatDisplayMode == .rich {
            ToolbarItem(placement: .principal) {
                // Build 128.1: double-tap the top bar to jump to the top of the
                // thread (oldest messages). Single tap still opens the context
                // popover inside compactStatusControl.
                compactStatusControl
                    .contentShape(Rectangle())
                    .onTapGesture(count: 2) {
                        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                        scrollToTop()
                    }
                    .accessibilityAction(named: "Scroll to top") {
                        scrollToTop()
                    }
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            HStack(spacing: Design.Spacing.sm) {
                catchUpButton
                canvasButton
            }
        }
    }

    /// Width-adaptive toolbar for iPad/Mac.
    /// When the chat column is wide enough, shows the full profile/model/timer
    /// arrangement. Under width pressure, collapses to the compact status chip
    /// so SwiftUI never synthesizes a `…` overflow menu.
    @ToolbarContentBuilder
    private var adaptiveToolbarContent: some ToolbarContent {
        // Build 84: pill moved from .topBarLeading to .principal. In the iPad
        // NavigationSplitView the leading slot is shared with the system
        // sidebar toggle, which squeezed the text to zero width even with
        // layoutPriority(1). .principal (same slot iPhone uses) gives the
        // pill its own width budget - proven working on iPhone.
        ToolbarItem(placement: .principal) {
            // Build 128.1: double-tap the top bar to jump to the top of the
            // thread (oldest messages) - same affordance as iPhone.
            ViewThatFits(in: .horizontal) {
                // Wide: profile + model + timer
                HStack(spacing: Design.Spacing.sm) {
                    profileChip
                    modelStatusChip
                    sessionTimerChip
                }
                // Medium: model + timer (drops profile chip)
                HStack(spacing: Design.Spacing.sm) {
                    modelStatusChip
                    sessionTimerChip
                }
                // Compact: same bounded chip as iPhone
                compactStatusControl
            }
            .contentShape(Rectangle())
            .onTapGesture(count: 2) {
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                scrollToTop()
            }
            .accessibilityAction(named: "Scroll to top") {
                scrollToTop()
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            ViewThatFits(in: .horizontal) {
                // Wide: Canvas + Settings
                HStack(spacing: Design.Spacing.sm) {
                    catchUpButton
                    canvasButton
                    GlassCircleButton(icon: "gearshape", accessibilityLabel: "Open settings") {
                        router.switchToTab(.settings)
                    }
                }
                // Compact: Canvas only (Settings accessible via sidebar)
                canvasButton
            }
        }
    }

    /// Canvas action button — shared across all toolbar compositions.
    private var canvasButton: some View {
        Button {
            showCanvas = true
        } label: {
            Image(systemName: "rectangle.on.rectangle")
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(canvasStore.activeArtifact != nil
                    ? Design.Brand.accent
                    : Design.Colors.secondaryForeground)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Open canvas")
    }

    /// Build 128.1: visible catch-up affordance. Pull-to-refresh exists
    /// (.refreshable) but is undiscoverable until you scroll to the very
    /// top; this button gives the same performForegroundCatchUp path a
    /// one-tap surface in the toolbar. Spins while a refresh is in flight.
    private var catchUpButton: some View {
        Button {
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            Task {
                guard !isCatchingUp else { return }
                isCatchingUp = true
                await chatStore.performForegroundCatchUp()
                isCatchingUp = false
            }
        } label: {
            if isCatchingUp {
                ProgressView()
                    .controlSize(.small)
                    .frame(width: Design.Size.iconSmall, height: Design.Size.iconSmall)
            } else {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: Design.Size.iconSmall, weight: .medium))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
        .buttonStyle(.plain)
        .frame(
            width: Design.Size.glassCircleButton,
            height: Design.Size.glassCircleButton
        )
        .contentShape(Circle())
        .accessibilityLabel("Catch up to latest messages")
    }

    /// Compact status control for iPhone principal toolbar slot.
    /// Shows connection dot + compact model name + context ring. Opens context popover on tap.
    /// Tapping the connection dot shows infra details.
    /// Width-bounded to prevent system overflow ellipsis.
    private var compactStatusControl: some View {
        HStack(spacing: 4) {
            // Connection dot — visual status indicator only.
            // Full infrastructure details are in Settings → Infrastructure.
            Circle()
                .fill(connectionIndicatorColor)
                .frame(width: 6, height: 6)

            // Model name + context ring with tap handler for context popover
            Button {
                showContextPopover.toggle()
            } label: {
                HStack(spacing: 4) {
                    if let model = displayedModelName {
                        Text(compactModelName(ModelNamePretty.prettyName(model)))
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(Design.Colors.foreground)
                            .lineLimit(1)
                            // Build 56: layoutPriority(1) so the model name
                            // never collapses to zero width in tight toolbar
                            // slots. The context ring has a fixed 16pt frame
                            // and is incompressible, so SwiftUI was shrinking
                            // the Text to nothing first - the iPad pill
                            // rendered as a bare green dot even though the
                            // model name was loaded (the iPhone principal
                            // slot had enough width, the iPad leading slot
                            // didn't).
                            .layoutPriority(1)
                    } else if modelStore.isLoading {
                        Text("Model…")
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                            .layoutPriority(1)
                    } else {
                        // Never render an unlabeled status capsule on iPad.
                        // It looked like a toggle in compact split-view widths
                        // and concealed both the failure and the way to retry.
                        Text(modelStore.isError ? "Model unavailable" : "Model")
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                            .layoutPriority(1)
                    }

                    contextRing(progress: contextProgress)
                }
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(Capsule().fill(Design.Colors.surface))
        .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
        .popover(isPresented: $showContextPopover) {
            contextPopoverContent
                .presentationCompactAdaptation(.popover)
        }
        .accessibilityLabel("Model and connection status")
    }

    @State private var showContextPopover = false
    @State private var showHeraldHub = false
    @State private var heraldHubInitialTab: HeraldSelectorSheet.Tab = .models

    /// `ModelStore.activeModel` is the authoritative source once populated —
    /// it's set from the `POST /v1/model` response after a direct switch, so
    /// preferring it here means the chip updates the moment a switch
    /// succeeds instead of waiting for the next command-catalog refresh.
    private var displayedModelName: String? {
        // Build 53: an EMPTY model name (not nil) must fall through to the
        // fallback labels, otherwise the iPad pill renders just the context
        // ring with no text - a bare circle that hides both the model and
        // the failure state. Treat blank/whitespace as "no model".
        let raw = modelStore.activeModel?.name ?? chatStore.activeModelName ?? hostStore.currentHost?.heraldModel
        guard let raw, !raw.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return raw
    }

    private var effectiveContextWindow: Int? {
        modelStore.activeModel?.contextWindow ?? chatStore.resolvedContextWindow(fallbackModelName: displayedModelName)
    }

    private var currentContextTokens: Int? {
        chatStore.currentContextTokens
    }

    /// Build 128.76: skill names surfaced for the TUI landing Skills panel.
    /// Pulls from ProfileStore's active profiles (the connector's /v1/profiles
    /// path reports real skillCount); falls back to nothing when profiles
    /// haven't loaded so the panel renders its loading hint.
    private var skillNames: [String] {
        profileStore.profiles.map { $0.name }
    }

    /// Combined content length of the streaming placeholder message.
    /// Observed to keep the scroll anchored during streaming (message count
    /// doesn't change, but content grows continuously).
    private var streamingContentLength: Int {
        guard let sid = chatStore.streamingMessageID,
              let msg = chatStore.conversation?.messages.first(where: { $0.id == sid })
        else { return 0 }
        return msg.content.count + msg.reasoning.count
    }

    /// Context usage as 0.0–1.0. Uses the relay-reported percentage when
    /// available (authored by the model itself via its real context window),
    /// falling back to a locally-computed estimate only when the relay has
    /// not yet delivered usage data.
    private var contextProgress: Double {
        // Authoritative: the relay reports context from the model's actual window
        if let percent = chatStore.conversation?.contextPercent {
            return percent / 100.0
        }
        // Fallback: local estimate from prompt tokens / inferred window
        guard let usedTokens = currentContextTokens,
              let maxCtx = effectiveContextWindow, maxCtx > 0
        else { return 0 }
        return min(Double(usedTokens) / Double(maxCtx), 1.0)
    }

    // MARK: - Profile chip

    private var profileChip: some View {
        Group {
            if !profileStore.profiles.isEmpty {
                Button {
                    heraldHubInitialTab = .profiles
                    showHeraldHub = true
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "brain.head.profile")
                        Text(profileStore.displayProfileName)
                            .font(.caption)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(.ultraThinMaterial, in: Capsule())
                }
            }
        }
    }

    // MARK: - Session timer chip

    private var sessionTimerChip: some View {
        Group {
            if let firstMessage = chatStore.conversation?.messages.first {
                let startTime = firstMessage.timestamp
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    let elapsed = context.date.timeIntervalSince(startTime)
                    if elapsed >= 0 {
                        Text(formatSessionDuration(elapsed))
                            .font(.system(size: 11, weight: .medium, design: .monospaced))
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 3)
                            .background(Capsule().fill(Design.Colors.surface))
                    }
                }
            }
        }
    }

    private func formatSessionDuration(_ interval: TimeInterval) -> String {
        let totalSeconds = Int(max(interval, 0))
        let minutes = totalSeconds / 60
        let seconds = totalSeconds % 60
        if minutes == 0 { return "\(seconds)s" }
        if minutes < 60 { return "\(minutes)m" }
        let hours = minutes / 60
        let remaining = minutes % 60
        return remaining == 0 ? "\(hours)h" : "\(hours)h \(remaining)m"
    }

    // MARK: - Compact chip: 🟢 model-name [ring%]

    private var modelStatusChip: some View {
        HStack(spacing: Design.Spacing.xs) {
            // Connection dot — visual status indicator only.
            // Full infrastructure details are in Settings → Infrastructure.
            Circle()
                .fill(connectionIndicatorColor)
                .frame(width: 6, height: 6)

            // Model name + context ring with tap handler for context popover
            Button {
                showContextPopover.toggle()
            } label: {
                HStack(spacing: Design.Spacing.xs) {
                    if let model = displayedModelName {
                        ViewThatFits(in: .horizontal) {
                            HStack(spacing: 4) {
                                chipModelText(ModelNamePretty.prettyName(model))
                                if modelStore.isLoading {
                                    ProgressView()
                                        .controlSize(.mini)
                                }
                            }
                            HStack(spacing: 4) {
                                chipModelText(compactModelName(ModelNamePretty.prettyName(model)))
                                if modelStore.isLoading {
                                    ProgressView()
                                        .controlSize(.mini)
                                }
                            }
                        }
                    } else if modelStore.isLoading {
                        // Build 30: the wide chip must never go blank during
                        // a catalog refresh.  Show a label with spinner so
                        // the iPad pill always has human-readable content.
                        HStack(spacing: 4) {
                            Text("Loading…")
                                .font(.system(size: 12, weight: .medium, design: .monospaced))
                                .foregroundStyle(Design.Colors.secondaryForeground)
                            ProgressView()
                                .controlSize(.mini)
                        }
                    } else if modelStore.isError {
                        HStack(spacing: 4) {
                            Image(systemName: "exclamationmark.triangle")
                                .font(.system(size: 10))
                                .foregroundStyle(Design.Colors.secondaryForeground)
                            Text("Model unavailable")
                                .font(.system(size: 12, weight: .medium, design: .monospaced))
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    } else {
                        Text("No model")
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }

                    contextRing(progress: contextProgress)
                }
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, 6)
        .background(
            Capsule().fill(Design.Colors.surface)
        )
        .overlay(
            Capsule().stroke(Design.Colors.border, lineWidth: 1)
        )
        .fixedSize(horizontal: true, vertical: false)
        .popover(isPresented: $showContextPopover) {
            contextPopoverContent
                .presentationCompactAdaptation(.popover)
        }
    }

    private func chipModelText(_ model: String) -> some View {
        Text(model)
            .font(.system(size: 12, weight: .medium, design: .monospaced))
            .foregroundStyle(Design.Colors.foreground)
            .lineLimit(1)
            .truncationMode(.tail)
            .minimumScaleFactor(0.8)
            .layoutPriority(1)
    }

    private func contextRing(progress: Double) -> some View {
        ZStack {
            Circle()
                .stroke(Design.Colors.divider, lineWidth: 2.5)
            Circle()
                .trim(from: 0, to: max(progress, 0.001))
                .stroke(contextColor(progress), style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .frame(width: 16, height: 16)
    }

    // MARK: - Popover: Context Window X of Y (%)

    private var contextPopoverContent: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.md) {
            HStack(spacing: Design.Spacing.xs) {
                Circle()
                    .fill(connectionIndicatorColor)
                    .frame(width: 7, height: 7)

                if modelStore.isLoading {
                    ProgressView()
                        .controlSize(.small)
                    Text("Loading models...")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                } else if let model = displayedModelName {
                    MarqueeText(text: ModelNamePretty.prettyName(model)) {
                        Text(ModelNamePretty.prettyName(model))
                            .font(.system(.subheadline, design: .monospaced, weight: .semibold))
                            .foregroundStyle(Design.Colors.foreground)
                            .lineLimit(1)
                    }
                } else if modelStore.isError {
                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        Text(modelStore.errorMessage ?? "Failed to load models")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Button {
                            Task { await modelStore.loadModels(force: true) }
                        } label: {
                            Label("Retry", systemImage: "arrow.clockwise")
                                .font(Design.Typography.callout)
                        }
                    }
                } else {
                    Text("Model unavailable")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            }

            if let maxCtx = effectiveContextWindow, maxCtx > 0 {
                let total = formatTokenCount(maxCtx)

                VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                    Text("Context Window")
                        .brandEyebrow()

                    if let usedTokens = currentContextTokens {
                        let progress = min(Double(usedTokens) / Double(maxCtx), 1.0)
                        let used = formatTokenCount(usedTokens)

                        HStack(alignment: .lastTextBaseline, spacing: 4) {
                            Text(used)
                                .font(.system(size: 24, weight: .semibold, design: .monospaced))
                                .foregroundStyle(Design.Colors.foreground)
                            Text("/")
                                .font(.system(size: 18, weight: .medium, design: .monospaced))
                                .foregroundStyle(Design.Colors.secondaryForeground)
                            Text(total)
                                .font(.system(size: 18, weight: .medium, design: .monospaced))
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }

                        HStack(spacing: Design.Spacing.sm) {
                            Capsule()
                                .fill(Design.Colors.surface)
                                .overlay(alignment: .leading) {
                                    GeometryReader { proxy in
                                        Capsule()
                                            .fill(contextColor(progress))
                                            .frame(width: max(proxy.size.width * progress, 3))
                                    }
                                }
                                .frame(height: 8)

                            Text("\(Int(progress * 100))%")
                                .font(.system(.caption, design: .monospaced, weight: .semibold))
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }

                        Text("\(max(maxCtx - usedTokens, 0).formatted()) prompt tokens remaining")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    } else {
                        Text(total)
                            .font(.system(size: 24, weight: .semibold, design: .monospaced))
                            .foregroundStyle(Design.Colors.foreground)

                        Text("Total window available now. Usage appears after the first Kallisti response.")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }
            } else {
                Text("Context window unavailable for the active model.")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }

            Divider()

            Button {
                showContextPopover = false
                // Let the popover finish dismissing before presenting the sheet
                Task {
                    try? await Task.sleep(for: .milliseconds(350))
                    heraldHubInitialTab = .models
                    showHeraldHub = true
                }
            } label: {
                HStack {
                    Image(systemName: "arrow.triangle.2.circlepath")
                        .font(.system(size: 13, weight: .medium))
                    Text("Switch Model")
                        .font(Design.Typography.callout)
                    Spacer()
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .foregroundStyle(Design.Brand.accent)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
        .frame(width: 230, alignment: .leading)
        .padding(.horizontal, Design.Spacing.lg)
        .padding(.vertical, Design.Spacing.lg)
    }


    private func contextColor(_ progress: Double) -> Color {
        if progress > 0.85 { return Design.Colors.danger }
        if progress > 0.65 { return Design.Colors.warning }
        return Design.Brand.primary
    }

    private func formatTokenCount(_ count: Int) -> String {
        if count >= 1_000_000 {
            return compactDecimal(Double(count) / 1_000_000, suffix: "M")
        } else if count >= 1_000 {
            return compactDecimal(Double(count) / 1_000, suffix: "K")
        }
        return "\(count)"
    }

    private func compactModelName(_ model: String) -> String {
        let limit = DeviceClass.isPhone ? 12 : 16
        guard model.count > limit else { return model }
        return String(model.prefix(limit)) + "…"
    }

    private func compactDecimal(_ value: Double, suffix: String) -> String {
        let rounded = (value * 10).rounded() / 10
        if rounded == floor(rounded) {
            return "\(Int(rounded))\(suffix)"
        }
        return String(format: "%.1f%@", rounded, suffix)
    }

    private var connectionIndicatorColor: Color {
        Self.connectionIndicatorColor(
            status: chatStore.connectionStatus,
            modelsReady: !modelStore.isLoading && modelStore.activeModel != nil,
            streamStalled: chatStore.stallSnapshot != nil
                || chatStore.streamingPhase == .stalled
                || chatStore.streamingPhase == .reconnecting
        )
    }

    private var connectionStatusLabel: String {
        if chatStore.connectionStatus == .connected,
           modelStore.isLoading || modelStore.activeModel == nil {
            return "Loading models"
        }
        // Build 116: never claim plain "Connected" while the model stream is
        // stalled or reconnecting - the banner and the pill must agree, or
        // the green dot + "Stream stalled" pair reads as a contradiction.
        if chatStore.connectionStatus == .connected,
           chatStore.stallSnapshot != nil
            || chatStore.streamingPhase == .stalled
            || chatStore.streamingPhase == .reconnecting {
            return "Stream stalled"
        }
        return chatStore.connectionStatus.displayLabel
    }

    nonisolated static func connectionIndicatorColor(
        status: ConnectionStatus,
        modelsReady: Bool,
        streamStalled: Bool = false
    ) -> Color {
        if status == .connected, !modelsReady {
            return .yellow
        }
        // Build 116: a stalled/reconnecting stream is not "fully operational".
        // Show warning yellow so the dot agrees with the stall banner instead
        // of a confident green next to "Stream stalled".
        if status == .connected, streamStalled {
            return .yellow
        }
        return status.dotColor
    }

    nonisolated static func shouldRedactTranscript(
        isLoading: Bool,
        messageCount: Int
    ) -> Bool {
        isLoading && messageCount == 0
    }

    /// Keep the active turn as one immutable visual tail. Server refreshes can
    /// insert persisted tool-boundary rows while the turn is still running;
    /// raw array order then makes its thought card jump through the thread.
    ///
    /// Build 128.52: order the transcript by TIMESTAMP, not raw array index.
    /// The array is the merge/insertion order, which can diverge from wall
    /// clock when a row's stream starts early but its terminal write lands
    /// late - a message stamped 7:59 PM used to render BELOW an 8:04 PM row
    /// simply because its completion arrived last (Curtis screenshot
    /// 2026-08-16 20:07). Sorting here fixes the visible order without
    /// touching the merge logic that builds the array. Stable sort: equal
    /// timestamps (same-second tool-boundary rows, optimistic user rows)
    /// keep their array-relative order.
    nonisolated static func transcriptRows(_ messages: [Message]) -> [Message] {
        let active = messages.last(where: { $0.isStreaming })
        let sorted = messages.enumerated().sorted { lhs, rhs in
            let lTime = lhs.element.timestamp.timeIntervalSince1970
            let rTime = rhs.element.timestamp.timeIntervalSince1970
            if lTime != rTime { return lTime < rTime }
            return lhs.offset < rhs.offset
        }.map(\.element)
        // The live placeholder stays pinned at the bottom of the visible
        // transcript even though its creation timestamp is the OLDEST part of
        // the turn - it is the current activity and must not jump through the
        // thread as server refreshes insert persisted tool rows.
        guard let active else { return sorted }
        return sorted.filter { $0.id != active.id } + [active]
    }

    // MARK: - Message List

    private var messageList: some View {
        ScrollViewReader { proxy in
            ScrollView(.vertical) {
                LazyVStack(spacing: Design.Spacing.md) {
                    // Top anchor for scrolling to empty state after /new
                    Color.clear
                        .frame(height: 1)
                        .id("top")

                    if let messages = chatStore.conversation?.messages {
                        ForEach(Self.transcriptRows(messages)) { message in
                            MessageBubble(
                                message: message,
                                textColorHex: settingsStore.settings.chatTextColorHex,
                                onRetry: { failedMessage in
                                    Task { await chatStore.retryMessage(failedMessage) }
                                },
                                onStartNewSession: {
                                    Task { await performClear() }
                                },
                                onDelete: { msg in
                                    chatStore.deleteMessage(msg)
                                },
                                onOpenCanvas: { msg in
                                    let sessionID = chatStore.conversation?.id.uuidString ?? "unknown"
                                    canvasStore.open(message: msg, sessionID: sessionID)
                                    showCanvas = true
                                }
                            )
                            .equatable()
                            // A message must retain one view identity for its
                            // entire lifetime.  Keying it by content/reasoning
                            // length recreated the stream row every token, which
                            // made the thought-process view bounce around its
                            // answer as LazyVStack recalculated its layout.
                            .id(message.id)
                        }
                    }

                    if showStatusCard {
                        StatusCardView(
                            connectionLabel: connectionStatusLabel,
                            messageCount: chatStore.conversation?.messages.count ?? 0,
                            conversationID: chatStore.conversation?.id,
                            tokenUsage: chatStore.lastTokenUsage,
                            dismissAction: { showStatusCard = false }
                        )
                        .transition(.opacity)
                    }

                    // Build 31: stable bottom sentinel.  Every scrollTo targets
                    // this fixed anchor instead of a mutable message UUID — a
                    // message row that gets removed (delete/retry/placeholder settle)
                    // would turn scrollTo into a silent no-op with no recovery.
                    Color.clear
                        .frame(height: 1)
                        .id("bottom")
                }
                .padding(.vertical, Design.Spacing.md)
            }
            .scrollDismissesKeyboard(.interactively)
            .redacted(
                reason: Self.shouldRedactTranscript(
                    isLoading: chatStore.isLoading,
                    messageCount: chatStore.conversation?.messages.count ?? 0
                ) ? .placeholder : []
            )
            .onTapGesture {
                isComposerFocused = false
            }
            .simultaneousGesture(
                // Build 31: user-initiated drag pauses auto-follow.  The
                // geometry handler below resets isUserScrolling when the user
                // manually scrolls back to the bottom or taps Jump to Latest.
                // No timer — the pause lasts until the user explicitly returns.
                DragGesture(minimumDistance: 16)
                    .onChanged { _ in
                        isUserScrolling = true
                    }
            )
            .onAppear { scrollProxy = proxy }
            .onScrollGeometryChange(for: Bool.self) { geometry in
                // Consider "near bottom" when the visible rect's bottom edge
                // is within 44pt of the content's bottom edge.
                let bottomY = geometry.contentOffset.y + geometry.visibleRect.size.height
                let contentBottom = geometry.contentSize.height + geometry.contentInsets.bottom
                return bottomY >= contentBottom - 44
            } action: { _, nearBottom in
                isNearBottom = nearBottom
                // Build 124: reaching the bottom by any means means the user
                // has seen the latest content — clear the catch-up count so
                // the pill does not linger.
                if nearBottom {
                    chatStore.pendingNewMessageCount = 0
                }
                // Once user manually scrolls to the bottom, resume auto-scroll
                if nearBottom && isUserScrolling {
                    isUserScrolling = false
                }
            }
            .overlay(alignment: .bottom) {
                // Build 124: cross-device catch-up pill. When a foreground
                // refresh or server-turn watch finds rows this device never
                // rendered (thread advanced on the desktop/iPad while this
                // phone was backgrounded or scrolled away), show "N new
                // messages" above the input bar instead of the plain arrow.
                // Tap jumps to latest and clears the count.
                if chatStore.pendingNewMessageCount > 0 && !isNearBottom {
                    Button {
                        isUserScrolling = false
                        chatStore.pendingNewMessageCount = 0
                        scrollToBottom(animate: false, force: true)
                    } label: {
                        HStack(spacing: 6) {
                            Image(systemName: "arrow.down")
                                .font(.system(size: 12, weight: .semibold))
                            Text(chatStore.pendingNewMessageCount == 1
                                 ? "1 new message"
                                 : "\(chatStore.pendingNewMessageCount) new messages")
                                .font(.system(size: 13, weight: .semibold))
                        }
                        .foregroundStyle(Design.Colors.foreground)
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.vertical, 8)
                        .background(
                            Capsule()
                                .fill(Design.Colors.surface)
                                .shadow(color: .black.opacity(0.25), radius: 6, y: 2)
                        )
                    }
                    .buttonStyle(.plain)
                    .padding(.bottom, Design.Spacing.sm)
                    .transition(.scale.combined(with: .opacity))
                } else if showScrollArrow {
                    // Jump-to-bottom arrow — visible only when actually scrolled up,
                    // not on incidental drags. Centered above the input bar.
                    Button {
                        isUserScrolling = false
                        scrollToBottom(animate: false, force: true)
                    } label: {
                        Image(systemName: "arrow.down.circle.fill")
                            .font(.system(size: 32))
                            .foregroundStyle(Design.Brand.accent)
                            .background(Circle().fill(Design.Colors.surface).shadow(radius: 4))
                    }
                    .buttonStyle(.plain)
                    .padding(.bottom, Design.Spacing.sm)
                    .transition(.scale.combined(with: .opacity))
                }
            }
            .animation(Design.Motion.standard, value: chatStore.pendingNewMessageCount)
            .animation(Design.Motion.standard, value: showScrollArrow)
            .scrollBounceBehavior(.basedOnSize)
            .refreshable {
                // Build 127.1: pull-to-catch-up. Drag down from the top of the
                // transcript to force a cross-device refresh (same path as the
                // foreground catch-up). Surfaces new rows and the "N new messages"
                // pill when another device advanced the thread.
                await chatStore.performForegroundCatchUp()
            }
        }
    }

    private var connectionBanner: some View {
        HStack(alignment: .center, spacing: Design.Spacing.sm) {
            Image(systemName: connectionBannerIcon)
                .foregroundStyle(connectionIndicatorColor)

            VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
                Text(connectionBannerTitle)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
                Text(connectionBannerMessage)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }

            Spacer()

            Button(connectionBannerActionLabel) {
                connectionBannerAction()
            }
            .font(Design.Typography.caption)
            .foregroundStyle(Design.Brand.accent)
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
        .padding(.horizontal, Design.Spacing.md)
        .padding(.top, Design.Spacing.md)
    }

    /// Subtle streaming phase indicator — shown when the stream reconnects
    /// or stalls, so the user knows the app hasn't frozen.
    ///
    /// Build 64: the banner is a TimelineView that ticks every second off
    /// `chatStore.stallSnapshot.observedAt`, so the elapsed-seconds counter
    /// advances in real time. The snapshot is captured by the store at the
    /// moment the stall was first observed, so connection state, retry
    /// count, and last activity are honest and stable instead of a frozen
    /// string. The banner auto-clears as soon as the store's
    /// `clearStall()` runs (any text/reasoning/tool delta, or
    /// .finished / .cancelled / .failed).
    ///
    /// `Design.Colors.tertiaryForeground` is the design token for the
    /// secondary metadata line; the warning color stays on the headline.
    private var streamingPhaseBanner: some View {
        TimelineView(.periodic(from: .now, by: 1.0)) { context in
            streamingPhaseBannerBody(at: context.date)
        }
    }

    /// Body of `streamingPhaseBanner`, factored out so the TimelineView
    /// closure stays a single-expression view (avoids the Swift
    /// `@ViewBuilder` type-inference trap when local `let` bindings
    /// share a closure with `if/let/else` branches).
    @ViewBuilder
    private func streamingPhaseBannerBody(at now: Date) -> some View {
        if let bannerLine = chatStore.stallBannerLine(now: now) {
            HStack(spacing: 6) {
                if bannerLine.isWatchdogStall {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 10))
                        .foregroundStyle(Design.Colors.warning)
                } else {
                    // Transport-level reconnect (snapshot captured from
                    // .reconnecting, not the no-progress watchdog).
                    ProgressView()
                        .scaleEffect(0.6)
                        .tint(Design.Colors.warning)
                }
                VStack(alignment: .leading, spacing: 1) {
                    Text("Stream stalled - \(bannerLine.elapsedSeconds)s elapsed - attempt \(bannerLine.attemptNumber)")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.warning)
                        .lineLimit(1)
                    HStack(spacing: 4) {
                        Image(systemName: "antenna.radiowaves.left.and.right")
                            .font(.system(size: 9))
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                        Text(bannerLine.connection.displayLabel)
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                        Text("-")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                        Text("last: \(bannerLine.lastActivity) (\(bannerLine.lastActivitySecondsAgo)s)")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                }
                Spacer()
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, 4)
            .background(Design.Colors.warning.opacity(0.08))
            .accessibilityLabel("Stream stalled, \(bannerLine.elapsedSeconds) seconds elapsed, attempt \(bannerLine.attemptNumber), connection \(bannerLine.connection.displayLabel)")
        }
    }

    /// Build 33: shown while a Hermes gateway restart is in flight. Sends are
    /// queued visibly (the composer still accepts input; ChatStore enqueues it
    /// with an explanatory system message) and the status dot reads
    /// "Restarting…".
    private var restartBanner: some View {
        HStack(spacing: 4) {
            ProgressView()
                .scaleEffect(0.6)
                .tint(Design.Colors.warning)
            Text("Hermes is restarting — your message will send when it's back.")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.warning)
            Spacer()
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, 4)
        .background(Design.Colors.warning.opacity(0.08))
    }

    // Build 128.50: queue status bar (Electron parity). Shows how many
    // messages are queued behind the active turn and whether they're HELD
    // (won't auto-fire) or will drain automatically when the turn ends.
    private var queueStatusBar: some View {
        HStack(spacing: Design.Spacing.sm) {
            Button {
                showQueueManager = true
            } label: {
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: chatStore.isQueueHeld ? "pause.circle.fill" : "list.bullet.below.rectangle")
                        .font(.system(size: 13))
                        .foregroundStyle(chatStore.isQueueHeld ? Design.Colors.warning : Design.Brand.accent)
                    Text(queueStatusText)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .lineLimit(1)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Open queue manager")
            .accessibilityHint("View, edit, or delete queued messages")
            Spacer()
            Button {
                chatStore.setQueueHeld(!chatStore.isQueueHeld)
            } label: {
                Text(chatStore.isQueueHeld ? "Release" : "Hold")
                    .font(Design.Typography.caption.weight(.semibold))
                    .foregroundStyle(chatStore.isQueueHeld ? Design.Brand.accent : Design.Colors.warning)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, 3)
                    .background(
                        Capsule()
                            .fill(Design.Colors.surface)
                            .overlay(
                                Capsule()
                                    .stroke(Design.Colors.border, lineWidth: 1)
                            )
                    )
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, 5)
        .background(chatStore.isQueueHeld ? Design.Colors.warning.opacity(0.10) : Design.Brand.accent.opacity(0.06))
    }

    private var queueStatusText: String {
        let count = chatStore.queuedCountForCurrentConversation
        if chatStore.isQueueHeld {
            return count > 0 ? "\(count) queued message\(count == 1 ? "" : "s") held" : "Queue held"
        }
        return count > 0 ? "\(count) queued message\(count == 1 ? "" : "s") - will send after this turn" : "Queue ready"
    }

    // D4: contextWarningBanner removed — was driven by fabricated percentage.

    private var connectionBannerIcon: String {
        switch hostStore.connectionState {
        case .online:
            return "desktopcomputer"
        case .offline:
            return "desktopcomputer.trianglebadge.exclamationmark"
        case .unreachable:
            return "wifi.exclamationmark"
        case .notConnected:
            return "desktopcomputer"
        }
    }

    private var connectionBannerTitle: String {
        switch hostStore.connectionState {
        case .online:
            return "Hermes host online"
        case .offline:
            return "Hermes host offline"
        case .unreachable:
            switch settingsStore.settings.relayConfiguration.connectionMode {
            case .tailscale:
                return "Tailnet relay unreachable"
            case .selfHostedRelay:
                return "Relay URL unreachable"
            }
        case .notConnected:
            return "No Hermes host connected"
        }
    }

    private var connectionBannerMessage: String {
        switch hostStore.connectionState {
        case .online:
            return "Your Hermes host is connected."
        case .offline:
            return settingsStore.settings.relayConfiguration.connectionMode.hostOfflineMessage
        case .unreachable:
            return hostStore.lastErrorMessage ?? settingsStore.settings.relayConfiguration.connectionMode.defaultOfflineMessage
        case .notConnected:
            return settingsStore.settings.relayConfiguration.connectionMode.notConnectedMessage
        }
    }

    private var connectionBannerActionLabel: String {
        switch hostStore.connectionState {
        case .online, .offline, .notConnected:
            return "Settings"
        case .unreachable:
            return settingsStore.settings.relayConfiguration.connectionMode.unreachableActionLabel
        }
    }

    private func connectionBannerAction() {
        switch hostStore.connectionState {
        case .unreachable:
            let mode = settingsStore.settings.relayConfiguration.connectionMode
            if let deepLink = mode.unreachableActionDeepLink,
               UIApplication.shared.canOpenURL(deepLink) {
                UIApplication.shared.open(deepLink)
            } else {
                Task {
                    await container.nativeGatewayClient?.resetConnection()
                    await hostStore.refresh()
                }
            }
        case .online, .offline, .notConnected:
            router.presentSheet(.settings)
        }
    }

    // MARK: - Actions

    private func sendMessage() {
        let content = chatStore.loadDraft(for: chatStore.conversation?.id ?? UUID()).trimmingCharacters(in: .whitespacesAndNewlines)
        let attachments = pendingAttachments
        guard !content.isEmpty || !attachments.isEmpty else { return }

        // Mode-aware pre-flight: when the relay is confirmed unreachable the
        // request would just fail. Each connection mode needs different guidance
        // (retry managed vs. reopen Tailscale vs. check a self-hosted URL), so
        // short-circuit the send and surface the right next step.
        if refuseSendIfUnreachable() {
            return
        }

        // Build 33 WSB: two-phase send. Freeze the draft now; the composer is
        // cleared only after Phase 1 (durable enqueue) returns — from that
        // point the on-disk outbox owns the text, so a force-quit at any
        // later moment cannot lose it.
        let clientMessageID = UUID()
        let conversationID = chatStore.conversation?.id

        isComposerFocused = false

        if settingsStore.settings.hapticFeedbackEnabled {
            HapticEngine.messageSent()
        }

        Task {
            if content.hasPrefix("/") && attachments.isEmpty {
                await dispatchTypedSlashCommand(content)
                return
            }

            // Phase 1: durable enqueue (synchronous in practice — no network).
            // The optimistic user row appears in the transcript immediately;
            // the outbox manifest is persisted before this returns.
            let record = await chatStore.enqueueMessage(
                content,
                attachments: attachments,
                clientMessageID: clientMessageID
            )
            guard record != nil else {
                // Rejected (empty/duplicate/over-limit) — keep the draft so
                // the user can see why nothing was sent.
                return
            }

            // Now safe to clear — the outbox has it.
            if let cid = conversationID { chatStore.clearDraft(for: cid) }
            pendingAttachments = []

            // Phase 2: submit (async; may queue behind an active job and
            // chain through the FIFO outbox).
            await chatStore.submitNextEligible(for: conversationID)
        }
    }

    // MARK: - TUI input (Build 128.78)

    /// Send path for the terminal prompt line. Mirrors sendMessage() but takes
    /// the text directly instead of reading the composer draft - the TUI owns
    /// its own input field. Slash commands route through the same dispatcher
    /// so /new, /clear, /title etc work identically in both modes.
    private func sendTerminalText(_ raw: String) {
        let content = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !content.isEmpty else { return }
        if refuseSendIfUnreachable() {
            return
        }
        let conversationID = chatStore.conversation?.id
        Task {
            if content.hasPrefix("/") {
                await dispatchTypedSlashCommand(content)
                return
            }
            let record = await chatStore.enqueueMessage(content, attachments: [])
            guard record != nil else { return }
            await chatStore.submitNextEligible(for: conversationID)
        }
    }

    @discardableResult
    private func refuseSendIfUnreachable() -> Bool {
        guard hostStore.connectionState == .unreachable else { return false }
        let mode = settingsStore.settings.relayConfiguration.connectionMode
        appendSystemMessage(mode.unreachableSendBlockedMessage)
        return true
    }

    func handleAttachmentResult(_ result: AttachmentResult) {
        guard pendingAttachments.count < PendingAttachment.maxAttachmentsPerMessage else { return }
        switch result {
        case .image(let image):
            if let attachment = PendingAttachment.image(image) {
                pendingAttachments.append(attachment)
            }
        case .file(let url):
            if let attachment = PendingAttachment.file(at: url) {
                pendingAttachments.append(attachment)
            }
        }
    }

    private func handleSlashCommand(_ command: SlashCommand, _ argument: String?) {
        // Agent pass-through: send the raw slash command text as a chat message.
        // The Herald agent processes it natively — same as Discord/Telegram.
        guard command.isLocal else {
            let outgoing: String
            if let arg = argument?.trimmingCharacters(in: .whitespacesAndNewlines), !arg.isEmpty {
                outgoing = "/\(command.name) \(arg)"
            } else {
                outgoing = "/\(command.name)"
            }
            Task { await sendSlashAsMessage(outgoing) }
            return
        }

        // Local commands dispatch synchronously in-app, so the composer is
        // consumed on tap.
        if let cid = chatStore.conversation?.id { chatStore.clearDraft(for: cid) }

        switch command.name {
        case "new", "reset":
            Task { await createNewSessionAndSwitch() }
        case "clear":
            showClearConfirmation = true

        case "history":
            showConversationHistory()

        case "save":
            chatStore.exportConversationToFile()
            appendSystemMessage("Conversation saved to Documents folder.")

        case "retry":
            Task { await performRetry() }

        case "undo":
            performUndo()

        case "title":
            if let name = argument?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
                chatStore.setConversationTitle(name)
                appendSystemMessage("Session title set: \(name)")
            } else {
                let current = chatStore.conversation?.title ?? "Kallisti"
                let id = chatStore.conversation.map { String($0.id.uuidString.prefix(8)) } ?? "—"
                appendSystemMessage("Session ID: \(id)…\nTitle: \(current)\nUsage: /title <your session title>")
            }

        default:
            break
        }
    }

    /// Sends a slash command as a regular chat message to the Herald agent.
    /// Clears the composer only after the send is accepted, so a draft refused
    /// for unreachability stays editable for retry.
    private func sendSlashAsMessage(_ text: String) async {
        if refuseSendIfUnreachable() { return }
        if let cid = chatStore.conversation?.id { chatStore.clearDraft(for: cid) }
        await chatStore.sendMessage(text, attachments: [])
        scrollToBottom()
    }

    private func dispatchTypedSlashCommand(_ text: String) async {
        let raw = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard raw.hasPrefix("/") else {
            await chatStore.sendMessage(raw, attachments: [])
            return
        }

        let body = String(raw.dropFirst())
        let parts = body.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true)
        guard let first = parts.first else { return }

        let commandName = String(first).lowercased()
        let argument = parts.count > 1 ? String(parts[1]) : nil
        let localCommand = (chatStore.commandCatalog + SlashCommand.localCommands)
            .first { $0.name == commandName && $0.suggestedArgument == nil && $0.isLocal }

        if let localCommand {
            handleSlashCommand(localCommand, argument)
        } else {
            await sendSlashAsMessage(raw)
        }
    }

    // D5: performCompress() deleted — compress command removed from catalogs.

    /// Creates a new session on the server and switches to it, replacing the
    /// current conversation. This is the correct "new chat" path — unlike
    /// `performClear()` which clears the current conversation in-place (and
    /// can race with relay state), this creates a fresh session with its own
    /// immutable UUID via `POST /sessions`.
    private func createNewSessionAndSwitch() async {
        showStatusCard = false
        // Cancel any in-flight streaming before switching sessions.
        // Without this, the new chat inherits the old session's streaming
        // state (activeStreams, streamingTask) and shows a stop button
        // for a chat that doesn't belong to it.
        // Build 128.50: creating a new chat must NOT interrupt a server-side
        // turn running on another device - only the composer Stop tap does.
        chatStore.cancelStreaming(interruptServerTurn: false)
        await sessionListStore.createNewSession()
        // Scroll to top — new session's conversation is empty
        withAnimation(Design.Motion.standard) {
            scrollProxy?.scrollTo("top", anchor: .top)
        }
    }

    private func performClear() async {
        do {
            try await chatStore.clearConversation()
            showStatusCard = false

            // Scroll to top anchor — conversation is now empty
            withAnimation(Design.Motion.standard) {
                scrollProxy?.scrollTo("top", anchor: .top)
            }
        } catch {
            let reason: String
            if (error as? URLError)?.code == .userAuthenticationRequired
                || "\(error)".contains("401") {
                reason = "Session expired — please re-pair your device"
            } else {
                reason = error.localizedDescription
            }
            appendSystemMessage("Couldn't start a new session — \(reason)")
        }
    }

    private func performRetry() async {
        if refuseSendIfUnreachable() { return }
        guard let messages = chatStore.conversation?.messages, !messages.isEmpty else {
            appendSystemMessage("No messages to retry.")
            return
        }

        // Find the last user message
        guard let lastUserIdx = messages.lastIndex(where: { $0.sender == .user }) else {
            appendSystemMessage("No user message found to retry.")
            return
        }

        let lastUserMessage = messages[lastUserIdx]
        let lastUserContent = lastUserMessage.content
        let attachments = lastUserMessage.attachments.compactMap(PendingAttachment.restore)
        let normalizedContent: String
        if !lastUserMessage.attachments.isEmpty,
           lastUserContent.range(of: #"^\[\d+ attachment"#, options: .regularExpression) != nil {
            normalizedContent = ""
        } else {
            normalizedContent = lastUserContent
        }

        // Remove everything from the last user message onward (user msg + assistant response + tool msgs)
        chatStore.conversation?.messages.removeSubrange(lastUserIdx...)

        appendSystemMessage("Retrying: \"\(String(lastUserContent.prefix(60)))\(lastUserContent.count > 60 ? "..." : "")\"")

        // Re-send the message through the full pipeline
        await chatStore.sendMessage(normalizedContent, attachments: attachments)
        scrollToBottom()
    }

    private func performUndo() {
        guard let messages = chatStore.conversation?.messages, !messages.isEmpty else {
            appendSystemMessage("No messages to undo.")
            return
        }

        // Walk backwards to find the last user message
        guard let lastUserIdx = messages.lastIndex(where: { $0.sender == .user }) else {
            appendSystemMessage("No user message found to undo.")
            return
        }

        let removedContent = messages[lastUserIdx].content
        let removedCount = messages.count - lastUserIdx

        // Truncate history to before the last user message
        chatStore.conversation?.messages.removeSubrange(lastUserIdx...)

        let remaining = chatStore.conversation?.messages.count ?? 0
        appendSystemMessage("Undid \(removedCount) message\(removedCount == 1 ? "" : "s"). Removed: \"\(String(removedContent.prefix(60)))\(removedContent.count > 60 ? "..." : "")\"\n\(remaining) message\(remaining == 1 ? "" : "s") remaining.")
    }

    private func showConversationHistory() {
        guard let messages = chatStore.conversation?.messages, !messages.isEmpty else {
            appendSystemMessage("No conversation history yet.")
            return
        }

        let previewLimit = 200
        var lines: [String] = ["── Conversation History ──"]
        var visibleIndex = 0

        for msg in messages {
            guard msg.sender == .user || msg.sender == .herald else { continue }
            visibleIndex += 1
            let role = msg.sender == .user ? "You" : "Kallisti"
            let preview = msg.content.prefix(previewLimit)
            let suffix = msg.content.count > previewLimit ? "..." : ""
            lines.append("[\(role) #\(visibleIndex)] \(preview)\(suffix)")
        }

        lines.append("\(visibleIndex) visible message\(visibleIndex == 1 ? "" : "s"), \(messages.count) total")
        appendSystemMessage(lines.joined(separator: "\n"))
    }

    private func appendSystemMessage(_ text: String) {
        let msg = Message(sender: .system, content: text, status: .delivered)
        chatStore.conversation?.messages.append(msg)
        scrollToBottom()
    }

    private func scrollToBottom(animate: Bool = true, force: Bool = false) {
        // Build 31: always target the stable "bottom" sentinel, never a
        // mutable message UUID.  If a message row is removed (delete, retry
        // truncation, placeholder settle), a message-id scrollTo silently
        // no-ops with no recovery path.  The sentinel is always present.
        //
        // Build 78.5: streaming auto-follow only fires when the user has
        // not taken scroll ownership (isUserScrolling). The old
        // `&& isNearBottom` gate was a deadlock: when the thinking bubble
        // grows below the viewport, onScrollGeometryChange flips
        // isNearBottom=false BEFORE the scroll lands, and the guard then
        // blocks the very scroll that would restore it - the user had to
        // tap Jump to Latest every turn. isUserScrolling is the correct
        // ownership signal (set by the drag gesture); geometry state is
        // not.
        guard force || !isUserScrolling else { return }
        autoScrollTask?.cancel()
        autoScrollTask = Task { @MainActor in
            if chatStore.isStreaming {
                try? await Task.sleep(for: Self.scrollDebounceInterval)
                guard !Task.isCancelled, (force || !isUserScrolling) else { return }
            }
            // Build 103: LazyVStack realizes off-screen rows asynchronously, so a
            // single scrollTo("bottom") lands short of the true bottom while rows
            // materialize -- the "down arrow needs multiple presses" regression.
            // Re-issue the scroll a few times after short settles to converge.
            for _ in 0..<4 {
                guard !Task.isCancelled else { return }
                if animate {
                    withAnimation(Design.Motion.standard) {
                        self.scrollProxy?.scrollTo("bottom", anchor: .bottom)
                    }
                } else {
                    self.scrollProxy?.scrollTo("bottom", anchor: .bottom)
                }
                try? await Task.sleep(for: .milliseconds(70))
            }
        }
    }

    /// Build 128.1: jump to the top of the transcript (oldest messages).
    /// Wired to double-tap on the top bar. Takes scroll ownership so a
    /// streamed message doesn't yank the view back down while the user is
    /// reading history; the Jump-to-Latest arrow becomes visible to return.
    private func scrollToTop() {
        isUserScrolling = true
        autoScrollTask?.cancel()
        withAnimation(Design.Motion.standard) {
            scrollProxy?.scrollTo("top", anchor: .top)
        }
    }

    private func scrollToResponseTop(_ id: UUID) {
        // Keep the start of the assistant response in view; without this,
        // a bottom-anchored ScrollView fights the growing message and feels flickery.
        var transaction = Transaction(animation: nil)
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            scrollProxy?.scrollTo(id, anchor: .top)
        }
    }

    // MARK: - Chat-Switch Overlay (Build 118)

    /// Frosted loading overlay shown while a session switch fetches the
    /// target conversation. Reuses the breathing coin + realtime status
    /// language from the launch surface so switching feels like progress,
    /// not a frozen old thread.
    private var chatSwitchOverlay: some View {
        ZStack {
            // Frosted backdrop.
            Design.Colors.background
                .opacity(0.72)
                .ignoresSafeArea()
                .background(.ultraThinMaterial)

            VStack(spacing: Design.Spacing.lg) {
                Image("KallistiSeal")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 96, height: 96)
                    .scaleEffect(switchCoinBreathes ? 1.07 : 0.96)
                    .opacity(switchCoinBreathes ? 1.0 : 0.82)
                    .shadow(
                        color: Design.Brand.accent.opacity(switchCoinBreathes ? 0.45 : 0.15),
                        radius: switchCoinBreathes ? 22 : 10
                    )

                VStack(spacing: Design.Spacing.xs) {
                    Text(chatSwitchStatusText)
                        .font(Design.Typography.sectionTitle)
                        .foregroundStyle(Design.Colors.foreground)
                    if let detail = chatStore.switchStatus {
                        Text(detail)
                            .font(Design.Typography.body)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }
            }
            .padding(Design.Spacing.xl)
        }
        .onAppear {
            withAnimation(.easeInOut(duration: 2.2).repeatForever(autoreverses: true)) {
                switchCoinBreathes = true
            }
        }
        .onDisappear {
            switchCoinBreathes = false
        }
        .transition(.opacity)
        .animation(Design.Motion.quickResponse, value: chatStore.isSwitchingConversation)
    }

    /// Realtime-ish status line for the switch overlay. Reads the same store
    /// states the banners use, so it reflects actual work in flight.
    private var chatSwitchStatusText: String {
        if chatStore.isLoading {
            return "Downloading conversation"
        }
        if modelStore.isLoading {
            return "Loading models"
        }
        if hostStore.connectionState != .online {
            return "Reconnecting to host"
        }
        return "Loading messages"
    }
}



// MARK: - Queue Manager Sheet (Build 128.52)

/// Build 128.52: shows every queued-but-not-yet-submitted message in the
/// current conversation with per-item edit and delete controls.
/// - Tap the left (queue) segment of the queue status bar to open it.
/// - Edit puts the item's text into a TextEditor; Save writes it back to the
///   durable outbox record and the optimistic transcript row.
/// - Delete removes the outbox record, its staged attachments, its optimistic
///   row, and the queued transcript bubble.
struct QueueManagerSheet: View {
    let queued: [ChatOutboxRecord]
    let onEdit: (UUID, String) -> Void
    let onDelete: (UUID) -> Void

    @Environment(\.dismiss) private var dismiss
    /// clientMessageID of the item currently being edited (nil = none).
    @State private var editingID: UUID?
    @State private var editText = ""
    /// clientMessageID of the item awaiting delete confirmation.
    @State private var confirmDeleteID: UUID?

    var body: some View {
        NavigationStack {
            Group {
                if queued.isEmpty {
                    VStack(spacing: Design.Spacing.md) {
                        Image(systemName: "list.bullet.below.rectangle")
                            .font(.system(size: 34))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Text("Nothing queued")
                            .font(Design.Typography.sectionTitle)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Text("Messages you queue behind the active turn show up here.")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    List {
                        if let editingID, let item = queued.first(where: { $0.clientMessageID == editingID }) {
                            Section("Edit") {
                                TextEditor(text: $editText)
                                    .frame(minHeight: 90)
                                    .padding(4)
                                    .overlay(
                                        RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                                            .stroke(Design.Colors.border, lineWidth: 1)
                                    )
                                HStack {
                                    Button("Cancel") {
                                        self.editingID = nil
                                        editText = ""
                                    }
                                    .buttonStyle(.bordered)
                                    Spacer()
                                    Button("Save") {
                                        onEdit(item.clientMessageID, editText)
                                        self.editingID = nil
                                        editText = ""
                                    }
                                    .buttonStyle(.borderedProminent)
                                    .disabled(editText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                                }
                                .font(Design.Typography.body.weight(.medium))
                            }
                        }

                        Section("Queued (\(queued.count))") {
                            ForEach(queued) { item in
                                queuedRow(item)
                            }
                        }
                    }
                    .listStyle(.insetGrouped)
                }
            }
            .navigationTitle("Queue")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private func queuedRow(_ item: ChatOutboxRecord) -> some View {
        HStack(alignment: .top, spacing: Design.Spacing.sm) {
            VStack(alignment: .leading, spacing: 2) {
                Text(item.cleanText.isEmpty ? "[attachment\(item.attachmentRefs.count == 1 ? "" : "s")]" : item.cleanText)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
                    .lineLimit(3)
                HStack(spacing: 4) {
                    if !item.attachmentRefs.isEmpty {
                        Image(systemName: "paperclip")
                            .font(.system(size: 9))
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                    }
                    Text(item.createdAt, format: .dateTime.hour().minute())
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                }
            }
            Spacer()
            if editingID == item.clientMessageID {
                Button {
                    self.editingID = nil
                    editText = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .buttonStyle(.plain)
            } else {
                Button {
                    editingID = item.clientMessageID
                    editText = item.cleanText
                } label: {
                    Image(systemName: "pencil.circle")
                        .font(.system(size: 18))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Edit queued message")
            }
            Button {
                if confirmDeleteID == item.clientMessageID {
                    onDelete(item.clientMessageID)
                    confirmDeleteID = nil
                    editingID = nil
                } else {
                    confirmDeleteID = item.clientMessageID
                }
            } label: {
                Image(systemName: confirmDeleteID == item.clientMessageID ? "trash.circle.fill" : "trash.circle")
                    .font(.system(size: 18))
                    .foregroundStyle(confirmDeleteID == item.clientMessageID ? Design.Colors.warning : Design.Colors.secondaryForeground)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(confirmDeleteID == item.clientMessageID ? "Tap again to delete" : "Delete queued message")
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .onTapGesture {
            // Tap outside the action buttons dismisses any pending delete
            // confirmation or edit without changing anything.
            confirmDeleteID = nil
        }
    }
}
