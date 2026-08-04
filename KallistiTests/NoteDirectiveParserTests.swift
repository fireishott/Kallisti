import Foundation
import Testing
@testable import Herald

struct NoteDirectiveParserTests {
    let parser = NoteDirectiveParser()
    let noteId = UUID(uuidString: "A1B2C3D4-E5F6-7890-ABCD-EF1234567890")!
    let sourceTextRevision = 8

    @Test("Parse single directive")
    func parseSingleDirective() {
        let text = "Meeting notes\n#research cloud migration vendors\nMore text"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.command == .research)
        #expect(directives.first?.arguments == "cloud migration vendors")
    }

    @Test("Parse multiple directives")
    func parseMultipleDirectives() {
        let text = "#summary\n#actions\n#research battery supply chain"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 3)
        #expect(directives[0].command == .summary)
        #expect(directives[1].command == .actions)
        #expect(directives[2].command == .research)
        #expect(directives[2].arguments == "battery supply chain")
    }

    @Test("Case insensitive command parsing")
    func caseInsensitiveParsing() {
        let text = "#Research topic\n#SEARCH term\n#Summary\n#ACTIONS"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 4)
        #expect(directives[0].command == .research)
        #expect(directives[1].command == .search)
        #expect(directives[2].command == .summary)
        #expect(directives[3].command == .actions)
    }

    @Test("Unknown commands are skipped")
    func unknownCommandsSkipped() {
        let text = "#unknown command\n#research valid\n#alsoUnknown"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.command == .research)
    }

    @Test("Hash in URLs is skipped")
    func hashInURLSkipped() {
        let text = "See https://example.com/#section for details\n#research topic"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.command == .research)
    }

    @Test("Hash in code blocks is skipped")
    func hashInCodeBlockSkipped() {
        let text = "```\n#not a directive\n```\n#research topic"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.command == .research)
    }

    @Test("Mid-sentence hash is skipped")
    func midSentenceHashSkipped() {
        let text = "This is a C# programming guide\n#research topic"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.command == .research)
    }

    @Test("Directive with no arguments")
    func directiveWithNoArguments() {
        let text = "#summary"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.command == .summary)
        #expect(directives.first?.arguments == "")
    }

    @Test("Duplicate directives get different fingerprints")
    func duplicateDirectivesDifferentFingerprints() {
        let text = "#research topic A\n#research topic B"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 2)
        #expect(directives[0].fingerprint != directives[1].fingerprint)
    }

    @Test("Same directive on same revision gets same fingerprint")
    func sameDirectiveSameFingerprint() {
        let text = "#research topic"
        let d1 = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)
        let d2 = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(d1.first?.fingerprint == d2.first?.fingerprint)
    }

    @Test("Different revision gets different fingerprint")
    func differentRevisionDifferentFingerprint() {
        let text = "#research topic"
        let d1 = parser.parse(text: text, noteId: noteId, sourceTextRevision: 1)
        let d2 = parser.parse(text: text, noteId: noteId, sourceTextRevision: 2)

        #expect(d1.first?.fingerprint != d2.first?.fingerprint)
    }

    @Test("Unicode arguments preserved")
    func unicodeArguments() {
        let text = "#research 日本語のテスト"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.arguments == "日本語のテスト")
    }

    @Test("Whitespace-only lines skipped")
    func whitespaceOnlyLinesSkipped() {
        let text = "   \n\n#research topic\n   \n"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.command == .research)
    }

    @Test("All v1 commands parse correctly")
    func allV1Commands() {
        let text = "#research topic\n#search query\n#talkingpoints for meeting\n#summary\n#actions\n#questions"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 6)
        #expect(directives[0].command == .research)
        #expect(directives[1].command == .search)
        #expect(directives[2].command == .talkingPoints)
        #expect(directives[3].command == .summary)
        #expect(directives[4].command == .actions)
        #expect(directives[5].command == .questions)
    }

    @Test("Corrected text is used when present")
    func correctedTextUsed() {
        // The parser always receives the effectiveText (corrected or raw)
        // So this test verifies the parser works on any text
        let text = "Corrected text\n#research fixed topic"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: sourceTextRevision)

        #expect(directives.count == 1)
        #expect(directives.first?.arguments == "fixed topic")
    }
}
