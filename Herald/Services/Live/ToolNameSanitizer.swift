import Foundation

/// Maps raw tool invocations from the Hermes event stream to lock-screen-safe labels.
/// Raw values can be full shell command lines; nothing path-like or argument-like may render.
enum ToolNameSanitizer {
    static func displayLabel(for raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "Working" }

        // Bare identifier: letters/digits/underscore/dash/dot only, no spaces or slashes.
        let identifier = trimmed.range(of: #"^[A-Za-z][A-Za-z0-9_.\-]{0,63}$"#, options: .regularExpression) != nil
        if identifier {
            switch trimmed.lowercased() {
            case "view_file", "read_file", "cat": return "Reading a file"
            case "search", "grep", "find": return "Searching"
            case "date": return "Checking the time"
            default:
                if trimmed.count > 25 { return String(trimmed.prefix(25)) + "\u{2026}" }
                return trimmed
            }
        }
        // Anything else (spaces, slashes, quotes, pipes, flags) is a command line.
        return "Running a command"
    }
}
