import Foundation

/// A segment of parsed markdown content — prose, fenced code block, image,
/// inline thinking block, tool call, or markdown table.
enum MarkdownSegment: Identifiable {
    case prose(id: UUID = UUID(), text: String)
    case codeBlock(id: UUID = UUID(), language: String?, code: String)
    case image(id: UUID = UUID(), url: URL, altText: String)
    case video(id: UUID = UUID(), url: URL, altText: String)
    case thinking(id: UUID = UUID(), content: String)
    case toolCall(id: UUID = UUID(), name: String, args: String?, result: String?)
    case table(id: UUID = UUID(), rows: [[String]])

    var id: UUID {
        switch self {
        case .prose(let id, _): return id
        case .codeBlock(let id, _, _): return id
        case .image(let id, _, _): return id
        case .video(let id, _, _): return id
        case .thinking(let id, _): return id
        case .toolCall(let id, _, _, _): return id
        case .table(let id, _): return id
        }
    }
}

// Regex for markdown images: ![alt text](url)
// nonisolated(unsafe) satisfies Swift 6.2 strict concurrency for global Regex.
nonisolated(unsafe) private let markdownImagePattern = /!\[([^\]]*)\]\(([^)]+)\)/
// HTML img tags: <img src="url"> or <img src="url"/> or <img src="url"></img>
nonisolated(unsafe) private let htmlImagePattern = /<img\s+src=["']?(https?:\/\/[^\s"'<>]+)["']?\s*\/?\s*>(\s*<\/img>)?/
// HTML video tags: <video src="url"> or <video src="url"></video>
nonisolated(unsafe) private let htmlVideoPattern = /<video[^>]+src=["']?(https?:\/\/[^\s"'<>]+)["']?[^>]*\/?\s*>/

/// Image file extensions the parser recognizes.
private let imageExtensions: Set<String> = ["png", "jpg", "jpeg", "gif", "webp", "svg"]

/// Known image hosting domains (always treated as images regardless of extension).
private let imageHostPatterns: [String] = ["fal.media", "fal-cdn", "replicate.delivery", "oaidalleapiprodscus"]

/// Video file extensions.
private let videoExtensions: Set<String> = ["mp4", "mov", "m4v", "webm", "mkv"]

/// Known video hosting URL patterns.
private let videoHostPatterns: [String] = ["youtube.com/watch", "youtu.be/", "vimeo.com/"]

/// Returns true if the URL looks like an image.
private func isImageURL(_ urlString: String) -> Bool {
    let lower = urlString.lowercased()
    // Check extension
    if let ext = URL(string: lower)?.pathExtension, imageExtensions.contains(ext) {
        return true
    }
    // Check known image hosts
    for host in imageHostPatterns {
        if lower.contains(host) { return true }
    }
    return false
}

/// Returns true if the URL looks like a video.
private func isVideoURL(_ urlString: String) -> Bool {
    let lower = urlString.lowercased()
    if let ext = URL(string: lower)?.pathExtension, videoExtensions.contains(ext) {
        return true
    }
    for host in videoHostPatterns {
        if lower.contains(host) { return true }
    }
    return false
}

/// A media match (image or video) found in prose text.
private struct MediaMatch: Comparable {
    let range: Range<String.Index>
    let url: String
    let alt: String
    let isVideo: Bool

    static func < (lhs: MediaMatch, rhs: MediaMatch) -> Bool {
        lhs.range.lowerBound < rhs.range.lowerBound
    }
}

/// Splits prose text into interleaved prose, image, and video segments.
/// Handles markdown ![alt](url), HTML <img>/<video> tags, and bare video URLs.
private func splitProseAndMedia(_ text: String) -> [MarkdownSegment] {
    var matches: [MediaMatch] = []

    for match in text.matches(of: markdownImagePattern) {
        matches.append(MediaMatch(range: match.range, url: String(match.2), alt: String(match.1), isVideo: false))
    }
    for match in text.matches(of: htmlImagePattern) {
        matches.append(MediaMatch(range: match.range, url: String(match.1), alt: "", isVideo: false))
    }
    for match in text.matches(of: htmlVideoPattern) {
        matches.append(MediaMatch(range: match.range, url: String(match.1), alt: "", isVideo: true))
    }

    // Detect bare video URLs (a line that is just a youtube/vimeo URL)
    let lines = text.components(separatedBy: "\n")
    for line in lines {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("http") && isVideoURL(trimmed),
           let lineRange = text.range(of: line) {
            matches.append(MediaMatch(range: lineRange, url: trimmed, alt: "", isVideo: true))
        }
    }

    matches.sort()

    guard !matches.isEmpty else {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? [] : [.prose(text: trimmed)]
    }

    var segments: [MarkdownSegment] = []
    var lastEnd = text.startIndex

    for media in matches {
        guard media.range.lowerBound >= lastEnd else { continue }

        let before = String(text[lastEnd..<media.range.lowerBound])
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if !before.isEmpty {
            segments.append(.prose(text: before))
        }

        if let url = URL(string: media.url), url.scheme == "http" || url.scheme == "https" {
            if media.isVideo || isVideoURL(media.url) {
                segments.append(.video(url: url, altText: media.alt))
            } else {
                segments.append(.image(url: url, altText: media.alt))
            }
        } else {
            let raw = String(text[media.range])
            segments.append(.prose(text: raw))
        }

        lastEnd = media.range.upperBound
    }

    let after = String(text[lastEnd...]).trimmingCharacters(in: .whitespacesAndNewlines)
    if !after.isEmpty {
        segments.append(.prose(text: after))
    }

    return segments
}

/// Parses markdown content into alternating prose, fenced code block, image,
/// thinking, tool-call, and table segments.
///
/// Prose segments retain inline markdown (`**bold**`, `` `code` ``, `[links]()`, etc.)
/// that `AttributedString(markdown:)` handles natively.
///
/// Markdown images (`![alt](url)`) are extracted as `.image` segments and rendered
/// separately as async-loaded images.
///
/// `<think>…</think>` blocks are extracted as `.thinking` segments.
///
/// During streaming, an unclosed fence at the end of content is still emitted as a
/// `.codeBlock` so the user sees code as it arrives.
func parseMarkdownSegments(_ content: String, isStreaming: Bool = false) -> [MarkdownSegment] {
    guard !content.isEmpty else { return [] }
    return extractThinkingBlocks(content, isStreaming: isStreaming)
}

/// Entry point used by MarkdownContentView when a message has tool activities.
func parseMarkdownSegments(_ content: String, isStreaming: Bool = false, toolActivities: [ToolActivity]) -> [MarkdownSegment] {
    var segments = parseMarkdownSegments(content, isStreaming: isStreaming)

    let toolSegments = toolActivities.map { activity -> MarkdownSegment in
        let label = activity.label
        if let parenIdx = label.firstIndex(of: "(") {
            let name = String(label[..<parenIdx])
            let argsWithParen = String(label[parenIdx...])
            return .toolCall(name: name, args: argsWithParen, result: nil)
        } else {
            return .toolCall(name: label, args: nil, result: nil)
        }
    }

    if !toolSegments.isEmpty {
        if let lastProseIdx = segments.lastIndex(where: {
            if case .prose = $0 { return true }
            return false
        }) {
            segments.insert(contentsOf: toolSegments, at: lastProseIdx)
        } else {
            segments = toolSegments + segments
        }
    }
    return segments
}

// MARK: - Thinking Block Extraction

/// Pre-pass that splits on `<think>…</think>` tags, parsing the non-thinking
/// parts through the standard fenced-block + image pipeline.
private func extractThinkingBlocks(_ text: String, isStreaming: Bool = false) -> [MarkdownSegment] {
    var segments: [MarkdownSegment] = []
    var remainder = text
    let openTag = "<think>"
    let closeTag = "</think>"

    while let openRange = remainder.range(of: openTag) {
        let before = String(remainder[..<openRange.lowerBound])
        if !before.isEmpty {
            segments.append(contentsOf: parseMarkdownWithoutThinking(before, isStreaming: isStreaming))
        }
        remainder = String(remainder[openRange.upperBound...])
        if let closeRange = remainder.range(of: closeTag) {
            let thinkContent = String(remainder[..<closeRange.lowerBound])
            segments.append(.thinking(content: thinkContent))
            remainder = String(remainder[closeRange.upperBound...])
        } else {
            // Unclosed think tag — treat rest as thinking (streaming-friendly)
            segments.append(.thinking(content: remainder))
            remainder = ""
        }
    }
    if !remainder.isEmpty {
        segments.append(contentsOf: parseMarkdownWithoutThinking(remainder, isStreaming: isStreaming))
    }
    return segments
}

// MARK: - Table Extraction

/// Detects consecutive lines starting with `|` as markdown tables.
private func extractTables(from prose: String, isStreaming: Bool = false) -> [MarkdownSegment] {
    var result: [MarkdownSegment] = []
    let lines = prose.components(separatedBy: "\n")
    var i = 0
    var textAccumulator: [String] = []

    while i < lines.count {
        let line = lines[i]
        if line.trimmingCharacters(in: .whitespaces).hasPrefix("|") {
            // Flush accumulated prose
            let text = textAccumulator.joined(separator: "\n")
            if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                result.append(contentsOf: splitProseAndMedia(text))
            }
            textAccumulator = []
            // Collect table lines
            var tableLines: [String] = []
            while i < lines.count && lines[i].trimmingCharacters(in: .whitespaces).hasPrefix("|") {
                tableLines.append(lines[i])
                i += 1
            }
            // Parse rows (skip separator lines: |---|---|)
            let rows = tableLines.compactMap { line -> [String]? in
                let cells = line.components(separatedBy: "|")
                    .dropFirst().dropLast()
                    .map { $0.trimmingCharacters(in: .whitespaces) }
                if cells.allSatisfy({ $0.allSatisfy({ $0 == "-" || $0 == ":" || $0 == " " }) }) {
                    return nil
                }
                return Array(cells)
            }
            if !rows.isEmpty {
                result.append(.table(rows: rows))
            }
        } else {
            textAccumulator.append(line)
            i += 1
        }
    }
    let remaining = textAccumulator.joined(separator: "\n")
    if !remaining.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
        result.append(contentsOf: splitProseAndMedia(remaining))
    }
    return result
}

// MARK: - Core Parser (no thinking blocks)

/// Parses fenced code blocks and images from prose that has no `<think>` tags.
private func parseMarkdownWithoutThinking(_ content: String, isStreaming: Bool = false) -> [MarkdownSegment] {
    guard !content.isEmpty else { return [] }

    let lines = content.components(separatedBy: "\n")
    var segments: [MarkdownSegment] = []
    var currentProse: [String] = []
    var currentCode: [String] = []
    var codeLanguage: String?
    var insideCodeBlock = false

    func flushProse() {
        guard !currentProse.isEmpty else { return }
        let text = currentProse.joined(separator: "\n")
        currentProse = []
        segments.append(contentsOf: extractTables(from: text, isStreaming: isStreaming))
    }

    for line in lines {
        if !insideCodeBlock {
            if line.hasPrefix("```") {
                flushProse()
                insideCodeBlock = true
                let langTag = String(line.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                codeLanguage = langTag.isEmpty ? nil : langTag
                currentCode = []
            } else {
                currentProse.append(line)
            }
        } else {
            if line.hasPrefix("```") {
                insideCodeBlock = false
                let code = currentCode.joined(separator: "\n")
                segments.append(.codeBlock(language: codeLanguage, code: code))
                currentCode = []
                codeLanguage = nil
            } else {
                currentCode.append(line)
            }
        }
    }

    if insideCodeBlock {
        let code = currentCode.joined(separator: "\n")
        if isStreaming || !code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            segments.append(.codeBlock(language: codeLanguage, code: code))
        } else {
            currentProse.append("```\(codeLanguage ?? "")")
            currentProse.append(contentsOf: currentCode)
            flushProse()
        }
    } else {
        flushProse()
    }

    return segments
}
