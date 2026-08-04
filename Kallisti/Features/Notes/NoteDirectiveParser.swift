import CryptoKit
import Foundation

/// Pure, deterministic, Sendable parser for note directives.
/// Runs on `userCorrectedText` when present, else raw text.
/// Emits stable directive IDs and normalized fingerprints.
struct NoteDirectiveParser: Sendable {
    /// Parse directives from recognized text.
    /// - Parameters:
    ///   - text: The text to parse (corrected OCR or raw)
    ///   - noteId: The note ID (for fingerprint generation)
    ///   - sourceTextRevision: The text revision (for fingerprint generation)
    /// - Returns: Array of parsed directives
    func parse(text: String, noteId: UUID, sourceTextRevision: Int) -> [NoteDirective] {
        var directives: [NoteDirective] = []
        let lines = text.components(separatedBy: .newlines)

        for (lineIndex, line) in lines.enumerated() {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard trimmed.hasPrefix("#") else { continue }

            // Skip # in URLs, code blocks, mid-sentence
            if isInURLContext(line: line, text: text) || isInCodeBlock(lineIndex: lineIndex, text: text) {
                continue
            }

            // Parse the directive
            let afterHash = trimmed.dropFirst()
            let components = afterHash.components(separatedBy: .whitespaces)
            guard let commandString = components.first?.lowercased(),
                  let command = NoteCommand.parse(commandString) else {
                // Unknown tag — skip (not sent as intent)
                continue
            }

            let arguments = components.dropFirst().joined(separator: " ").trimmingCharacters(in: .whitespaces)

            // Calculate source range
            let sourceRange = NSRange(
                location: text.distance(from: text.startIndex, to: line.startIndex),
                length: line.count
            )

            // Generate fingerprint
            let fingerprint = generateFingerprint(
                noteId: noteId,
                sourceTextRevision: sourceTextRevision,
                command: command,
                arguments: arguments,
                sourceRange: sourceRange
            )

            let directive = NoteDirective(
                id: fingerprint,
                command: command,
                arguments: arguments,
                sourceRange: sourceRange,
                fingerprint: fingerprint
            )
            directives.append(directive)
        }

        return directives
    }

    /// Generate a normalized fingerprint for deduplication.
    /// Same input always produces the same fingerprint — OCR churn and save retries cannot duplicate execution.
    private func generateFingerprint(
        noteId: UUID,
        sourceTextRevision: Int,
        command: NoteCommand,
        arguments: String,
        sourceRange: NSRange
    ) -> String {
        let input = "\(noteId.uuidString)|\(sourceTextRevision)|\(command.rawValue)|\(arguments)|\(sourceRange.location):\(sourceRange.length)"
        let data = Data(input.utf8)
        let hash = SHA256.hash(data: data)
        return hash.map { String(format: "%02x", $0) }.joined()
    }

    /// Check if a # appears in a URL context (e.g., `https://example.com/#section`).
    private func isInURLContext(line: String, text: String) -> Bool {
        // Simple heuristic: if the line contains "://", the # is likely a fragment
        return line.contains("://")
    }

    /// Check if a line is inside a code block (fenced with ```).
    private func isInCodeBlock(lineIndex: Int, text: String) -> Bool {
        let lines = text.components(separatedBy: .newlines)
        var inCodeBlock = false
        for i in 0..<lineIndex {
            if lines[i].trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                inCodeBlock.toggle()
            }
        }
        return inCodeBlock
    }
}
