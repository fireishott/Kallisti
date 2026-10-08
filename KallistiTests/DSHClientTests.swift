import Testing
import Foundation
@testable import Kallisti

@Suite("DSH assistant media")
struct DSHClientTests {
    @Test("MEDIA image directive becomes an inline attachment")
    func mediaDirectiveBecomesInlineAttachment() throws {
        let source = "Here is the finished poster.\n\nMEDIA: /home/fihadmin/hailey-engagement/louisville-poster-v2/out/FINAL_inline.jpg\n"
        let resolved = DSHClient.resolveAssistantMedia(in: source) { path in
            URL(string: "https://fih-ai-host.tail4d497d.ts.net/dsh/phone/v1/media?p=" + path.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!)
        }

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
