import Foundation
import Testing
@testable import Kallisti

/// The chat prose is block markdown (headings, lists, quotes) on top of the
/// inline markdown the attributed-string parser handles. These pin the
/// classification that the renderer depends on, including the cases that must
/// stay paragraphs so ordinary prose is not turned into a list.
@Suite("Markdown block lines")
struct MarkdownBlockLineTests {
    @Test("headings carry their level and drop the hashes")
    func headings() {
        let lines = parseBlockLines("""
        # Title
        ## Current hard-match list
        ### Model Y
        """)
        #expect(lines == [
            .heading(level: 1, text: "Title"),
            .heading(level: 2, text: "Current hard-match list"),
            .heading(level: 3, text: "Model Y"),
        ])
    }

    @Test("a hash without a space is prose, not a heading")
    func hashTagsAreNotHeadings() {
        #expect(parseBlockLines("##NoSpace") == [.paragraph("##NoSpace")])
        #expect(parseBlockLines("####### seven") == [.paragraph("####### seven")])
    }

    @Test("bullets, ordered items, quotes, rules and paragraphs are separated")
    func blockKinds() {
        let lines = parseBlockLines("""
        - Factory-installed FSD
          - nested under the first
        1. first step
        2) second step
        > quoted line
        ---
        plain prose
        """)
        #expect(lines == [
            .bullet(text: "Factory-installed FSD", depth: 0),
            .bullet(text: "nested under the first", depth: 1),
            .numbered(number: "1", text: "first step", depth: 0),
            .numbered(number: "2", text: "second step", depth: 0),
            .quote(text: "quoted line"),
            .rule,
            .paragraph("plain prose"),
        ])
    }

    @Test("a dash or a year is prose when markdown would not read it as a list")
    func falsePositivesStayProse() {
        // No space after the dash: a negative number, not a bullet.
        #expect(parseBlockLines("-5 degrees overnight") == [.paragraph("-5 degrees overnight")])
        // A decimal keeps the digits from becoming an ordered marker.
        #expect(parseBlockLines("3.5% growth") == [.paragraph("3.5% growth")])
        // An em dash is not a bullet marker.
        #expect(parseBlockLines("— the answer") == [.paragraph("— the answer")])
    }

    @Test("blank lines stay blank and every other line is classified")
    func blankLines() {
        let lines = parseBlockLines("alpha\n\nbeta")
        #expect(lines == [.paragraph("alpha"), .blank, .paragraph("beta")])
    }

    @Test("inline markdown inside a block line is preserved for the inline parser")
    func inlineMarkdownSurvives() {
        let lines = parseBlockLines("- `4811006` is the right deal")
        #expect(lines == [.bullet(text: "`4811006` is the right deal", depth: 0)])
    }

    @Test("paragraph runs stay one text block and structural lines break them")
    func paragraphRunsAreGrouped() {
        let groups = groupProseLines(parseBlockLines("""
        alpha
        beta

        ## Head
        - item
        """))
        #expect(groups == [
            .text("alpha\nbeta\n"),
            .block(.heading(level: 2, text: "Head")),
            .block(.bullet(text: "item", depth: 0)),
        ])
    }

    @Test("a blank line between paragraphs is kept, not rendered as a gap block")
    func blankLinesStayInsideTheRun() {
        let groups = groupProseLines(parseBlockLines("alpha\n\nbeta"))
        #expect(groups == [.text("alpha\n\nbeta")])
    }
}
