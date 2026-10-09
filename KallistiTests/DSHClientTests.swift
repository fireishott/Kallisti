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
