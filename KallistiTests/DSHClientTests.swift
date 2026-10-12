import Testing
import Foundation
@testable import Kallisti

@Suite("DSH assistant media")
struct DSHClientTests {
    /// Mirrors DSHClient's own encoder: an absolute path becomes a bare,
    /// strictly-encoded /phone/v1/media URL.
    private static func mediaProvider(_ path: String) -> URL? {
        let strict = CharacterSet(charactersIn:
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
        guard path.hasPrefix("/"),
              let encoded = path.addingPercentEncoding(withAllowedCharacters: strict) else { return nil }
        return URL(string: "https://fih-ai-host.tail4d497d.ts.net/dsh/phone/v1/media?p=" + encoded)
    }

    /// The parser captures an image target with `[^)]+` and renders an image only
    /// when URL(string:) then carries an http(s) scheme.
    private static func imageTargets(in text: String) -> [String] {
        let pattern = /!\[([^\]]*)\]\(([^)]+)\)/
        return text.matches(of: pattern).map { String($0.2) }
    }

    @Test("bracketed local path renders as a fetchable inline image")
    func bracketedLocalPathBecomesInlineImage() throws {
        let source = "Here are the headshots:\n\n![JJ Square Headshot](</home/fihadmin/jj_headshot_square.jpg>)\n"
        let resolved = DSHClient.resolveAssistantMedia(in: source, mediaURLProvider: Self.mediaProvider)

        #expect(resolved.attachments.isEmpty)
        #expect(resolved.text.contains("Here are the headshots"))
        let target = try #require(Self.imageTargets(in: resolved.text).first)
        #expect(!target.contains("<") && !target.contains(">"))
        let url = try #require(URL(string: target))
        #expect(url.scheme == "https")
        #expect(url.path == "/dsh/phone/v1/media")
        #expect(url.query?.contains("p=%2Fhome%2Ffihadmin%2Fjj_headshot_square.jpg") == true)
    }

    @Test("unbracketed local path also becomes an inline image")
    func bareLocalPathBecomesInlineImage() throws {
        let source = "![poster](/home/fihadmin/out/poster.png)"
        let resolved = DSHClient.resolveAssistantMedia(in: source, mediaURLProvider: Self.mediaProvider)

        let target = try #require(Self.imageTargets(in: resolved.text).first)
        #expect(URL(string: target)?.scheme == "https")
    }

    @Test("a path with parentheses cannot truncate the captured URL")
    func parenthesisedPathIsEncoded() throws {
        let source = "![x](</home/fihadmin/out (2)/shot.jpg>)"
        let resolved = DSHClient.resolveAssistantMedia(in: source, mediaURLProvider: Self.mediaProvider)

        let target = try #require(Self.imageTargets(in: resolved.text).first)
        #expect(!target.contains("(") && !target.contains(")") && !target.contains(" "))
        #expect(URL(string: target)?.scheme == "https")
    }

    @Test("two images on one line both resolve")
    func multipleImagesOnOneLine() throws {
        let source = "![a](</tmp/a.jpg>) then ![b](</tmp/b.jpg>)"
        let resolved = DSHClient.resolveAssistantMedia(in: source, mediaURLProvider: Self.mediaProvider)

        let targets = Self.imageTargets(in: resolved.text)
        #expect(targets.count == 2)
        for target in targets {
            #expect(URL(string: target)?.scheme == "https")
        }
        #expect(resolved.text.hasPrefix("![a](https"))
    }

    @Test("an already-valid bracketed URL just loses its brackets")
    func bracketedRemoteURLIsUnwrapped() throws {
        let source = "![chart](<https://example.com/chart.png>)"
        let resolved = DSHClient.resolveAssistantMedia(in: source, mediaURLProvider: Self.mediaProvider)

        #expect(resolved.text == "![chart](https://example.com/chart.png)")
        let target = try #require(Self.imageTargets(in: resolved.text).first)
        #expect(URL(string: target)?.scheme == "https")
    }

    @Test("an unresolvable reference stays visible instead of vanishing")
    func unresolvableReferenceIsLeftAlone() {
        let source = "![x](<~/pictures/shot.jpg>)"
        let resolved = DSHClient.resolveAssistantMedia(in: source, mediaURLProvider: Self.mediaProvider)

        #expect(resolved.text == source)
        #expect(resolved.attachments.isEmpty)
    }

    @Test("MEDIA image directive still becomes an attachment")
    func mediaDirectiveBecomesInlineAttachment() throws {
        let source = "Here is the finished poster.\n\nMEDIA: /home/fihadmin/hailey-engagement/louisville-poster-v2/out/FINAL_inline.jpg\n"
        let resolved = DSHClient.resolveAssistantMedia(in: source, mediaURLProvider: Self.mediaProvider)

        #expect(resolved.text == "Here is the finished poster.")
        #expect(resolved.attachments.count == 1)
        let image = try #require(resolved.attachments.first)
        #expect(image.kind == "image")
        #expect(image.fileName == "FINAL_inline.jpg")
        #expect(image.mimeType == "image/jpeg")
        #expect(image.mediaURL?.path == "/dsh/phone/v1/media")
    }

    @Test("MEDIA directive rejects non-image files")
    func mediaDirectiveLeavesNonImageInText() {
        let source = "MEDIA: /home/fihadmin/report.pdf"
        let resolved = DSHClient.resolveAssistantMedia(in: source) { _ in
            URL(string: "https://example.com/media")
        }

        #expect(resolved.attachments.isEmpty)
        #expect(resolved.text == source)
    }
}


// MARK: - Active-model precedence (the pill must not revert to the host default)

@Suite("DSH active model selection")
struct DSHActiveSelectionTests {
    private typealias Pick = (provider: String, model: String)

    private let grace: TimeInterval = 120
    private let hostDefault: (provider: String?, model: String?) = ("ninerouter", "ds/deepseek-v4-flash")

    @Test("an unapplied pick wins over everything")
    func pendingWins() {
        let chosen = DSHClient.resolveActiveSelection(
            pending: ("ninerouter", "cx/gpt-5.6-terra"),
            session: ("ninerouter", "MM/minimax-m3"),
            acknowledged: ("ninerouter", "MM/minimax-m3"),
            acknowledgedAt: Date(),
            now: Date(),
            grace: grace,
            hostDefault: hostDefault
        )
        #expect(chosen.provider == "ninerouter")
        #expect(chosen.model == "cx/gpt-5.6-terra")
    }

    @Test("with nothing selected the host default is shown")
    func fallsBackToHostDefault() {
        let chosen = DSHClient.resolveActiveSelection(
            pending: nil, session: nil, acknowledged: nil, acknowledgedAt: nil,
            now: Date(), grace: grace, hostDefault: hostDefault
        )
        #expect(chosen.model == "ds/deepseek-v4-flash")
    }

    /// The reported flip: pick a model, the catalog read races the projection,
    /// and the pill reverts to the model that was active before the switch.
    @Test("a fresh pick outranks a stale session projection")
    func freshPickBeatsStaleProjection() {
        let switchedAt = Date()
        let chosen = DSHClient.resolveActiveSelection(
            pending: nil,
            session: ("ninerouter", "MM/minimax-m3"),
            acknowledged: ("ninerouter", "cx/gpt-5.6-terra"),
            acknowledgedAt: switchedAt,
            now: switchedAt.addingTimeInterval(3),
            grace: grace,
            hostDefault: hostDefault
        )
        #expect(chosen.model == "cx/gpt-5.6-terra", "the pill must keep the model the user picked")
    }

    @Test("the projection wins again once the pick is no longer fresh")
    func stalePickYieldsToProjection() {
        let switchedAt = Date()
        let chosen = DSHClient.resolveActiveSelection(
            pending: nil,
            session: ("ninerouter", "MM/minimax-m3"),
            acknowledged: ("ninerouter", "cx/gpt-5.6-terra"),
            acknowledgedAt: switchedAt,
            now: switchedAt.addingTimeInterval(grace + 1),
            grace: grace,
            hostDefault: hostDefault
        )
        #expect(chosen.model == "MM/minimax-m3", "a switch made elsewhere must win once the window closes")
    }

    @Test("when the projection agrees there is nothing to override")
    func agreeingProjectionIsUsed() {
        let chosen = DSHClient.resolveActiveSelection(
            pending: nil,
            session: ("ninerouter", "cx/gpt-5.6-terra"),
            acknowledged: ("ninerouter", "cx/gpt-5.6-terra"),
            acknowledgedAt: Date(),
            now: Date(),
            grace: grace,
            hostDefault: hostDefault
        )
        #expect(chosen.model == "cx/gpt-5.6-terra")
    }

    /// A fresh Session has no committed projection. Answering with the host
    /// default here is the other half of the flip.
    @Test("a pick is shown when the session has no projection yet")
    func pickShownWhenProjectionAbsent() {
        let chosen = DSHClient.resolveActiveSelection(
            pending: nil,
            session: nil,
            acknowledged: ("ninerouter", "cx/gpt-5.6-terra"),
            acknowledgedAt: Date(),
            now: Date(),
            grace: grace,
            hostDefault: hostDefault
        )
        #expect(chosen.model == "cx/gpt-5.6-terra", "must not fall through to the host default")
    }

    @Test("provider-only differences are still a disagreement")
    func providerOnlyChangeCounts() {
        let switchedAt = Date()
        let chosen = DSHClient.resolveActiveSelection(
            pending: nil,
            session: ("ninerouter", "shared-name"),
            acknowledged: ("MM", "shared-name"),
            acknowledgedAt: switchedAt,
            now: switchedAt.addingTimeInterval(1),
            grace: grace,
            hostDefault: hostDefault
        )
        #expect(chosen.provider == "MM")
    }
}

// MARK: - Cron on the DSH backend

@Suite("CronStore DSH source")
@MainActor
struct CronStoreDSHTests {
    private static func job(_ id: String, _ name: String, _ schedule: String) -> CronStore.CronJob {
        CronStore.CronJob(
            id: id, name: name, schedule: schedule, prompt: "prompt for \(id)",
            enabled: true, lastRun: nil, nextRun: nil, lastResult: "scheduled"
        )
    }

    /// Before this provider existed, a DSH-mode Cron view fell through to the
    /// relay, whose /v1/cron reads Hermes' jobs.json - a file DSH never writes.
    /// It answered `jobs: []`, so the view claimed "No Cron Jobs" no matter how
    /// many reminders the harness held.
    @Test("the DSH source supplies the list instead of the relay")
    func dshProviderWins() async {
        let store = CronStore(apiClient: nil, accessTokenProvider: { nil })
        store.dshCronProvider = {
            [Self.job("s1", "Morning digest", "every 1d"), Self.job("s2", "Call Kristie", "at 22:00")]
        }

        await store.loadJobs()

        #expect(store.jobs.count == 2)
        #expect(store.jobs.map(\.id) == ["s1", "s2"])
        #expect(store.errorMessage == nil, "a DSH read must not report a relay failure")
    }

    @Test("an empty DSH list is a real empty, not an error")
    func dshEmptyIsNotAnError() async {
        let store = CronStore(apiClient: nil, accessTokenProvider: { nil })
        store.dshCronProvider = { [] }

        await store.loadJobs()

        #expect(store.jobs.isEmpty)
        #expect(store.errorMessage == nil)
    }

    @Test("a failing DSH read surfaces its own error")
    func dshFailureSurfaces() async {
        struct Boom: Error {}
        let store = CronStore(apiClient: nil, accessTokenProvider: { nil })
        store.dshCronProvider = { throw Boom() }

        await store.loadJobs()

        #expect(store.errorMessage != nil)
    }

    /// Guard: without a DSH source the relay path is untouched.
    @Test("relay path still reports a missing connection")
    func relayPathUnchanged() async {
        let store = CronStore(apiClient: nil, accessTokenProvider: { nil })

        await store.loadJobs()

        #expect(store.jobs.isEmpty)
        #expect(store.errorMessage?.contains("Not connected") == true)
    }

    /// A DSH reminder is Session-scoped with no enable flag or cron rule, so a
    /// write must fail loudly rather than land on the wrong backend.
    @Test("creating a schedule on DSH is refused")
    func createRefusedOnDSH() async {
        let store = CronStore(apiClient: nil, accessTokenProvider: { nil })
        store.dshCronProvider = { [] }

        do {
            try await store.createJob(name: "n", schedule: "0 9 * * *", prompt: "p")
            Issue.record("expected the DSH write to be refused")
        } catch {
            #expect(error is CronStore.CronStoreError)
        }
    }

    @Test("toggling a schedule on DSH is refused")
    func toggleRefusedOnDSH() async {
        let store = CronStore(apiClient: nil, accessTokenProvider: { nil })
        let job = Self.job("s1", "n", "every 1d")
        store.dshCronProvider = { [job] }

        do {
            try await store.toggleJob(job)
            Issue.record("expected the DSH toggle to be refused")
        } catch {
            #expect(error is CronStore.CronStoreError)
        }
    }

    @Test("the refusal message names the backend")
    func refusalMessageIsActionable() {
        let error = CronStore.CronStoreError.unsupportedOnDSH("Creating a schedule")
        #expect(error.errorDescription?.contains("DSH") == true)
        #expect(error.errorDescription?.contains("Creating a schedule") == true)
    }
}
