import Foundation

/// Represents file changes detected by git after Herald completes a coding task.
///
/// The connector captures `git diff` before and after each job to isolate
/// exactly what Herald modified — no Herald framework changes required.
struct CodeDiff: Codable, Hashable, Sendable {
    let files: [FileDiff]
    let summary: String

    var isEmpty: Bool { files.isEmpty }
    var fileCount: Int { files.count }
    var totalAdditions: Int { files.reduce(0) { $0 + $1.additions } }
    var totalDeletions: Int { files.reduce(0) { $0 + $1.deletions } }
}

struct FileDiff: Codable, Hashable, Sendable, Identifiable {
    let path: String
    let status: String   // "modified", "added", "deleted", "renamed"
    let additions: Int
    let deletions: Int
    let patch: String

    var id: String { path }

    var fileName: String {
        (path as NSString).lastPathComponent
    }

    var directoryPath: String {
        let dir = (path as NSString).deletingLastPathComponent
        return dir.isEmpty ? "" : dir
    }

    var statusIcon: String {
        switch status {
        case "added": return "plus.circle.fill"
        case "deleted": return "minus.circle.fill"
        case "renamed": return "arrow.right.circle.fill"
        default: return "pencil.circle.fill"
        }
    }

    var statusColor: String {
        switch status {
        case "added": return "green"
        case "deleted": return "red"
        case "renamed": return "blue"
        default: return "orange"
        }
    }
}

// MARK: - Inline diff parser (native gateway path)

/// Parses the ANSI-colored inline unified diff text shipped by the Hermes
/// gateway (`tool.complete` → `inline_diff`, rendered by
/// `agent/display.py` `render_edit_diff_with_delta`/`_summarize_rendered_diff_sections`)
/// into the app's `CodeDiff` model.
///
/// The gateway renders each diff section as:
/// ```
///   ┊ review diff
/// a/foo.swift → b/foo.swift
/// @@ -1,5 +1,8 @@ ...
/// -removed line
/// +added line
///  context line
/// ```
/// with ANSI color escapes around every line. This parser strips the escapes,
/// splits per-file sections on the `a/X → b/Y` headers, counts +/- lines, and
/// reconstructs a standard unified diff patch for `InlineDiffView` to render.
enum CodeDiffParser {

    /// Parse one or more ANSI-colored inline diff blocks into a CodeDiff.
    /// Diffs arriving across multiple `tool.complete` events are merged by
    /// file path so a multi-file turn produces a single collapsible card.
    static func parse(accumulatedTexts: [String]) -> CodeDiff? {
        var merged: [String: FileDiff] = [:]
        for text in accumulatedTexts {
            guard let parsed = parse(single: text) else { continue }
            for file in parsed.files {
                if var existing = merged[file.path] {
                    existing = FileDiff(
                        path: file.path,
                        status: file.status,
                        additions: existing.additions + file.additions,
                        deletions: existing.deletions + file.deletions,
                        patch: existing.patch + "\n" + file.patch
                    )
                    merged[file.path] = existing
                } else {
                    merged[file.path] = file
                }
            }
        }
        guard !merged.isEmpty else { return nil }
        let files = merged.values.sorted { $0.path < $1.path }
        return CodeDiff(files: files, summary: summary(for: files))
    }

    /// Parse a single ANSI-colored inline diff block.
    static func parse(single text: String) -> CodeDiff? {
        let clean = stripANSI(text)
        let lines = clean.components(separatedBy: "\n")
        var files: [FileDiff] = []
        var current: FileDiffBuilder? = nil

        for rawLine in lines {
            let line = rawLine.trimmingCharacters(in: .whitespacesAndNewlines)
            if line.isEmpty || line.contains("┊ review diff") { continue }

            // File section header: "a/foo.swift → b/foo.swift",
            // "/dev/null → b/foo.swift" (added), "a/foo.swift → /dev/null" (deleted)
            if let (fromPath, toPath) = parseFileHeader(line) {
                if let c = current { files.append(c.build()) }
                current = FileDiffBuilder(fromPath: fromPath, toPath: toPath)
                continue
            }

            guard var c = current else { continue }
            if line.hasPrefix("@@") {
                c.lines.append(line)
            } else if line.hasPrefix("+") {
                c.additions += 1
                c.lines.append(line)
            } else if line.hasPrefix("-") {
                c.deletions += 1
                c.lines.append(line)
            } else {
                // Context line (gateway renders as " line" with leading space)
                c.lines.append(line)
            }
            current = c
        }
        if let c = current { files.append(c.build()) }

        guard !files.isEmpty else { return nil }
        return CodeDiff(files: files, summary: summary(for: files))
    }

    // MARK: Helpers

    private static func parseFileHeader(_ line: String) -> (from: String, to: String)? {
        let parts = line.components(separatedBy: " → ")
        guard parts.count == 2 else { return nil }
        let from = parts[0].trimmingCharacters(in: .whitespaces)
        let to = parts[1].trimmingCharacters(in: .whitespaces)
        // Header must look like a path on at least one side.
        guard !from.isEmpty, !to.isEmpty,
              from.hasPrefix("a/") || from == "/dev/null",
              to.hasPrefix("b/") || to == "/dev/null" else { return nil }
        return (from, to)
    }

    private static func stripANSI(_ text: String) -> String {
        // Strips \x1b[...m (SGR) escape sequences.
        var result = ""
        var iterator = text.makeIterator()
        var inEscape = false
        while let ch = iterator.next() {
            if inEscape {
                if ch == "m" { inEscape = false }
                continue
            }
            if ch == "\u{1B}" {
                inEscape = true
                continue
            }
            result.append(ch)
        }
        return result
    }

    private static func summary(for files: [FileDiff]) -> String {
        let fileCount = files.count
        let add = files.reduce(0) { $0 + $1.additions }
        let del = files.reduce(0) { $0 + $1.deletions }
        var parts = ["\(fileCount) file\(fileCount == 1 ? "" : "s") changed"]
        if add > 0 { parts.append("\(add) insertion\(add == 1 ? "" : "s")(+)") }
        if del > 0 { parts.append("\(del) deletion\(del == 1 ? "" : "s")(-)") }
        return parts.joined(separator: ", ")
    }

    /// Accumulates lines for one file section, then materializes a FileDiff
    /// with a standard unified diff patch (headers skipped by InlineDiffView).
    private struct FileDiffBuilder {
        let path: String
        let status: String
        var additions = 0
        var deletions = 0
        var lines: [String] = []

        init(fromPath: String, toPath: String) {
            if fromPath == "/dev/null" {
                self.status = "added"
                self.path = toPath.hasPrefix("b/") ? String(toPath.dropFirst(2)) : toPath
            } else if toPath == "/dev/null" {
                self.status = "deleted"
                self.path = fromPath.hasPrefix("a/") ? String(fromPath.dropFirst(2)) : fromPath
            } else {
                self.status = "modified"
                self.path = toPath.hasPrefix("b/") ? String(toPath.dropFirst(2)) : toPath
            }
        }

        func build() -> FileDiff {
            let patch = lines.joined(separator: "\n")
            return FileDiff(
                path: path,
                status: status,
                additions: additions,
                deletions: deletions,
                patch: patch
            )
        }
    }
}
