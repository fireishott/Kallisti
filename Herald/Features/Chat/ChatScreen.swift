import SwiftUI
import UIKit

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
    @Binding var isSessionDrawerOpen: Bool

    @State private var messageText = ""
    @State private var pendingAttachments: [PendingAttachment] = []
    @State private var showClearConfirmation = false
    @State private var showStatusCard = false
    @State private var scrollProxy: ScrollViewProxy?
    @FocusState private var isComposerFocused: Bool

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
        ZStack {
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
                if chatStore.isStreaming, chatStore.streamingPhase == .stalled || chatStore.streamingPhase == .reconnecting {
                    streamingPhaseBanner
                }
                // Build 33: a gateway restart supersedes transport banners —
                // the stream was suspended on purpose, so explain why.
                if chatStore.restartInProgress {
                    restartBanner
                }
                messageList
                ChatInputBar(
                    text: $messageText,
                    pendingAttachments: $pendingAttachments,
                    isStreaming: chatStore.isStreaming,
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
                        let frozenText = messageText
                        let frozenAttachments = pendingAttachments
                        let conversationID = chatStore.conversation?.id
                        Task {
                            guard chatStore.queueNextMessage(text: frozenText, attachments: frozenAttachments) != nil else { return }
                            messageText = ""
                            pendingAttachments = []
                            await chatStore.submitNextEligible(for: conversationID)
                        }
                    },
                    onAttach: { showAttachmentPicker = true },
                    onSlashCommand: handleSlashCommand
                )
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbarContent }
        .toolbarBackground(.hidden, for: .navigationBar)
        .task {
            chatStore.setPollingEnabled(true)
            await hostStore.refresh()
            await chatStore.loadConversationIfNeeded()
            // The active profile belongs to the connector, not the local chat
            // session cache. Refresh it whenever Chat becomes active so a
            // stale pre-pairing value such as ".hermes" is never retained in
            // the composer after the host reconnects or changes profile.
            await profileStore.loadProfiles(force: true)
            await modelStore.loadModels()
            // Build 33 WSB: reconcile the durable outbox — settle accepted
            // jobs against the relay, resubmit items whose backoff elapsed,
            // and submit queued items for this conversation.
            await chatStore.recoverOutbox()
            // Scroll to most recent messages after loading
            try? await Task.sleep(for: .milliseconds(150))
            scrollToBottom()
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
                scrollToBottom()
            }
        }
        .onChange(of: chatStore.streamingMessageID) { old, new in
            if old != nil && new == nil {
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
                        withAnimation(Design.Motion.standard) {
                            scrollProxy?.scrollTo(lastID, anchor: .bottom)
                        }
                    }
                }
            }
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
        .sheet(isPresented: $showCanvas) {
            CanvasView(store: canvasStore, onDismiss: { showCanvas = false })
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
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
        ToolbarItem(placement: .principal) {
            compactStatusControl
        }
        ToolbarItem(placement: .topBarTrailing) {
            canvasButton
        }
    }

    /// Width-adaptive toolbar for iPad/Mac.
    /// When the chat column is wide enough, shows the full profile/model/timer
    /// arrangement. Under width pressure, collapses to the compact status chip
    /// so SwiftUI never synthesizes a `…` overflow menu.
    @ToolbarContentBuilder
    private var adaptiveToolbarContent: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
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
        }
        ToolbarItem(placement: .topBarTrailing) {
            ViewThatFits(in: .horizontal) {
                // Wide: Canvas + Settings
                HStack(spacing: Design.Spacing.sm) {
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
                        Text(compactModelName(model))
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(Design.Colors.foreground)
                            .lineLimit(1)
                    } else if modelStore.isLoading {
                        Text("Model…")
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                    } else {
                        // Never render an unlabeled status capsule on iPad.
                        // It looked like a toggle in compact split-view widths
                        // and concealed both the failure and the way to retry.
                        Text(modelStore.isError ? "Model unavailable" : "Model")
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
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
        modelStore.activeModel?.name ?? chatStore.activeModelName ?? hostStore.currentHost?.heraldModel
    }

    private var effectiveContextWindow: Int? {
        modelStore.activeModel?.contextWindow ?? chatStore.resolvedContextWindow(fallbackModelName: displayedModelName)
    }

    private var currentContextTokens: Int? {
        chatStore.currentContextTokens
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
                                chipModelText(model)
                                if modelStore.isLoading {
                                    ProgressView()
                                        .controlSize(.mini)
                                }
                            }
                            HStack(spacing: 4) {
                                chipModelText(compactModelName(model))
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
                    Text(model)
                        .font(.system(.subheadline, design: .monospaced, weight: .semibold))
                        .foregroundStyle(Design.Colors.foreground)
                        .lineLimit(1)
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
        chatStore.connectionStatus.dotColor
    }

    private var connectionStatusLabel: String {
        chatStore.connectionStatus.displayLabel
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
                        ForEach(messages) { message in
                            MessageBubble(
                                message: message,
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
            .redacted(reason: chatStore.isLoading ? .placeholder : [])
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
                // Once user manually scrolls to the bottom, resume auto-scroll
                if nearBottom && isUserScrolling {
                    isUserScrolling = false
                }
            }
            .overlay(alignment: .bottom) {
                // Jump-to-bottom arrow — visible only when actually scrolled up,
                // not on incidental drags. Centered above the input bar.
                if showScrollArrow {
                    Button {
                        isUserScrolling = false
                        scrollToBottom()
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
            .animation(Design.Motion.standard, value: showScrollArrow)
            .scrollBounceBehavior(.basedOnSize)
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
    private var streamingPhaseBanner: some View {
        HStack(spacing: 4) {
            if chatStore.streamingPhase == .stalled {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 10))
                    .foregroundStyle(Design.Colors.warning)
                Text("Stream stalled — retrying…")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.warning)
            } else if chatStore.streamingPhase == .reconnecting {
                ProgressView()
                    .scaleEffect(0.6)
                    .tint(Design.Colors.warning)
                Text("Reconnecting…")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.warning)
            }
            Spacer()
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, 4)
        .background(Design.Colors.warning.opacity(0.08))
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
                Task { await hostStore.refresh() }
            }
        case .online, .offline, .notConnected:
            router.presentSheet(.settings)
        }
    }

    // MARK: - Actions

    private func sendMessage() {
        let content = messageText.trimmingCharacters(in: .whitespacesAndNewlines)
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
            messageText = ""
            pendingAttachments = []

            // Phase 2: submit (async; may queue behind an active job and
            // chain through the FIFO outbox).
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
        messageText = ""

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
        messageText = ""
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
        chatStore.cancelStreaming()
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

    private func scrollToBottom() {
        // Build 31: always target the stable "bottom" sentinel, never a
        // mutable message UUID.  If a message row is removed (delete, retry
        // truncation, placeholder settle), a message-id scrollTo silently
        // no-ops with no recovery path.  The sentinel is always present.
        //
        // During streaming, debounce scroll requests — multiple onChange
        // handlers can fire in the same run loop.  A single debounce Task
        // coalesces them into one scroll per ~100ms.
        if chatStore.isStreaming {
            guard !isUserScrolling else { return }
            autoScrollTask?.cancel()
            autoScrollTask = Task { @MainActor in
                try? await Task.sleep(for: Self.scrollDebounceInterval)
                guard !Task.isCancelled, !isUserScrolling else { return }
                withAnimation(Design.Motion.standard) {
                    scrollProxy?.scrollTo("bottom", anchor: .bottom)
                }
            }
            return
        }
        // Non-streaming: respect user scroll position.  Don't yank a user
        // who is reading history just because a poll merge changed the count.
        guard !isUserScrolling else { return }
        withAnimation(Design.Motion.standard) {
            scrollProxy?.scrollTo("bottom", anchor: .bottom)
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
}
