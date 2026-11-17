import Foundation

struct SentenceBoundary {
    let text: String
    let endIndex: String.Index
    let isComplete: Bool
}

struct SpeechDivergenceMetrics: Sendable {
    var sentencesSpoken: Int = 0
    var sentencesTotal: Int = 0
    var charactersSpoken: Int = 0
    var charactersTotal: Int = 0
    var hadDivergence: Bool = false
}

struct SpeechTextRenderer {
    /// Sentence-terminating characters (Western + CJK).
    private static let terminators = CharacterSet(charactersIn: ".!?。！？")

    /// Extract the next stable sentence boundary from streaming text.
    /// Returns nil if no complete sentence is found.
    static func findSentenceBoundary(in text: String) -> SentenceBoundary? {
        guard !text.isEmpty else { return nil }

        var lastTerminatorIndex: String.Index?
        var lastTerminatorEndIndex: String.Index?

        for i in text.indices {
            let char = text[i]
            let scalar = char.unicodeScalars.first!
            if terminators.contains(scalar) {
                // Must be followed by whitespace or end-of-string to be a real boundary
                let nextIndex = text.index(after: i)
                if nextIndex == text.endIndex {
                    lastTerminatorIndex = i
                    lastTerminatorEndIndex = nextIndex
                } else {
                    let nextChar = text[nextIndex]
                    if nextChar.isWhitespace || nextChar.isNewline {
                        lastTerminatorIndex = i
                        lastTerminatorEndIndex = nextIndex
                    }
                }
            }
        }

        guard let terminatorIndex = lastTerminatorIndex,
              let endIndex = lastTerminatorEndIndex else { return nil }

        let sentenceText = String(text[..<endIndex]).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !sentenceText.isEmpty else { return nil }

        return SentenceBoundary(
            text: sentenceText,
            endIndex: endIndex,
            isComplete: endIndex == text.endIndex
        )
    }

    /// Extract all sentence boundaries from completed text (for divergence comparison).
    static func findAllSentences(in text: String) -> [String] {
        let rendered = render(text)
        var sentences: [String] = []
        var current = ""

        for char in rendered {
            current.append(char)
            let scalar = char.unicodeScalars.first!
            if terminators.contains(scalar) {
                let trimmed = current.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty {
                    sentences.append(trimmed)
                }
                current = ""
            }
        }

        let remaining = current.trimmingCharacters(in: .whitespacesAndNewlines)
        if !remaining.isEmpty {
            sentences.append(remaining)
        }

        return sentences
    }

    /// Convert canonical Hermes message text to speakable text.
    static func render(_ text: String) -> String {
        var result = text

        // Remove code blocks
        result = result.replacingOccurrences(
            of: #"```[\s\S]*?```"#,
            with: "[code block omitted]",
            options: .regularExpression
        )

        // Remove inline code
        result = result.replacingOccurrences(
            of: #"`[^`]+`"#,
            with: "",
            options: .regularExpression
        )

        // Remove URLs
        result = result.replacingOccurrences(
            of: #"https?://\S+"#,
            with: "",
            options: .regularExpression
        )

        // Remove Markdown images
        result = result.replacingOccurrences(
            of: #"!\[[^\]]*\]\([^)]+\)"#,
            with: "",
            options: .regularExpression
        )

        // Convert headers to pauses (multiline: ^ matches start of each line)
        result = applyMultilineRegex(
            result,
            pattern: #"^#{1,6}\s+"#,
            replacement: "\n"
        )

        // Convert list markers (multiline)
        result = applyMultilineRegex(
            result,
            pattern: #"^\s*[-*+]\s+"#,
            replacement: ""
        )

        // Remove bold/italic markers
        result = result.replacingOccurrences(
            of: #"[*_]{1,3}"#,
            with: "",
            options: .regularExpression
        )

        // Collapse multiple newlines into pauses
        result = result.replacingOccurrences(
            of: #"\n{3,}"#,
            with: "\n\n",
            options: .regularExpression
        )

        return result.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func applyMultilineRegex(_ text: String, pattern: String, replacement: String) -> String {
        guard let regex = try? NSRegularExpression(pattern: pattern, options: .anchorsMatchLines) else {
            return text
        }
        let range = NSRange(text.startIndex..., in: text)
        return regex.stringByReplacingMatches(in: text, range: range, withTemplate: replacement)
    }
}
