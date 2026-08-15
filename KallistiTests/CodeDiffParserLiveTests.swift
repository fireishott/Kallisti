import Testing
import Foundation
@testable import Kallisti

struct CodeDiffParserLiveTests {
    @Test("Parser handles real gateway ANSI truecolor output")
    func realGatewayFormat() {
        // Simulates what _render_inline_unified_diff emits: truecolor SGR + reset per line
        let diff = "\u{1b}[38;2;180;160;255ma/ChatInputBar.swift → b/ChatInputBar.swift\u{1b}[0m\n"
            + "\u{1b}[38;2;120;120;140m@@ -137,6 +137,8 @@\u{1b}[0m\n"
            + "\u{1b}[38;2;255;255;255;48;2;84;42;21m-HStack(alignment: .bottom, spacing: 4)\u{1b}[0m\n"
            + "\u{1b}[38;2;255;255;255;48;2;21;84;42m+HStack(alignment: .bottom, spacing: 8)\u{1b}[0m\n"
            + "\u{1b}[38;2;150;150;150m context line\u{1b}[0m"
        let parsed = CodeDiffParser.parse(single: diff)
        #expect(parsed != nil)
        #expect(parsed?.fileCount == 1)
        #expect(parsed?.files.first?.path == "ChatInputBar.swift")
        #expect(parsed?.files.first?.additions == 1)
        #expect(parsed?.files.first?.deletions == 1)
    }

    @Test("Parser handles review-diff banner prefix")
    func bannerPrefix() {
        let diff = "  ┊ review diff\n"
            + "\u{1b}[38;2;180;160;255ma/F.swift → b/F.swift\u{1b}[0m\n"
            + "\u{1b}[38;2;120;120;140m@@ -1 +1 @@\u{1b}[0m\n"
            + "\u{1b}[38;2;255;255;255;48;2;84;42;21m-old\u{1b}[0m\n"
            + "\u{1b}[38;2;255;255;255;48;2;21;84;42m+new\u{1b}[0m"
        let parsed = CodeDiffParser.parse(single: diff)
        #expect(parsed != nil)
        #expect(parsed?.files.first?.path == "F.swift")
    }

    @Test("Parser handles raw unified diff result (--- a/ +++ b/)")
    func rawUnifiedDiffFormat() {
        // write_file/patch result.diff ships this shape (file_operations PatchResult)
        let diff = "--- a/F.swift\n+++ b/F.swift\n@@ -1,3 +1,4 @@\n struct S {\n-    let a = 1\n+    let a = 1\n+    let b = 2\n }\n"
        let parsed = CodeDiffParser.parse(single: diff)
        #expect(parsed != nil, "raw unified diff should parse")
        #expect(parsed?.fileCount == 1)
        #expect(parsed?.files.first?.path == "F.swift")
        #expect(parsed?.files.first?.additions == 1)
        #expect(parsed?.files.first?.deletions == 1)
    }

    @Test("Parser handles raw unified diff with /dev/null new file")
    func rawUnifiedDiffAddedFile() {
        let diff = "--- /dev/null\n+++ b/New.swift\n@@ -0,0 +1,2 @@\n+let x = 1\n+let y = 2\n"
        let parsed = CodeDiffParser.parse(single: diff)
        #expect(parsed != nil)
        #expect(parsed?.files.first?.path == "New.swift")
        #expect(parsed?.files.first?.status == "added")
        #expect(parsed?.files.first?.additions == 2)
    }

    @Test("Multi-file raw diff parses every file")
    func rawMultiFile() {
        let diff = "--- a/A.swift\n+++ b/A.swift\n@@ -1 +1 @@\n-oldA\n+newA\n"
            + "--- a/B.swift\n+++ b/B.swift\n@@ -1 +1 @@\n-oldB\n+newB\n"
        let parsed = CodeDiffParser.parse(single: diff)
        #expect(parsed?.fileCount == 2)
        #expect(parsed?.files.map(\\.path).sorted() == ["A.swift", "B.swift"])
    }

    @Test("NativeToolCompletePayload resultDiff falls back to result.diff")
    func resultDiffFallbackDecode() throws {
        // Gateway sends inline_diff absent, result object carries diff
        let data = Data(#"{"tool_id":"tc-edit","name":"write_file","result":{"success":true,"diff":"--- a/F.swift\n+++ b/F.swift\n@@ -1 +1 @@\n-old\n+new"},"duration_s":0.5}"#.utf8)
        let complete = try JSONDecoder().decode(NativeToolCompletePayload.self, from: data)
        #expect(complete.inlineDiff == nil)
        #expect(complete.resultDiff?.contains("+++ b/F.swift") == true)
    }

    @Test("Message codeDiff persists through Codable round-trip")
    func codeDiffPersists() throws {
        let diff = CodeDiff(files: [
            FileDiff(path: "F.swift", status: "modified", additions: 1, deletions: 1, patch: "@@ -1 +1 @@\n-old\n+new")
        ], summary: "1 file changed")
        let message = Message(id: UUID(), sender: .herald, content: "done", codeDiff: diff)
        let data = try JSONEncoder().encode(message)
        let decoded = try JSONDecoder().decode(Message.self, from: data)
        #expect(decoded.codeDiff != nil)
        #expect(decoded.codeDiff?.fileCount == 1)
        #expect(decoded.codeDiff?.files.first?.path == "F.swift")
    }
}
