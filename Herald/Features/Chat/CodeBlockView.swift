import SwiftUI

/// Renders a fenced code block with monospaced font, distinct background,
/// optional language label, and a copy-to-clipboard button.
///
/// Build 122: diff-aware. When the block looks like a unified diff (language
/// tag "diff"/"patch", or lines starting with `diff --git`, `---`, `+++`,
/// `@@`, `+`, `-`), it renders like a real diff card: green backgrounds on
/// added lines, red on removed lines, a blue-tinted hunk header, and the file
/// path header lines dimmed. Plain code blocks keep syntax highlighting.
struct CodeBlockView: View {
    let language: String?
    let code: String

    @State private var didCopy = false

    private var isDiff: Bool {
        let lang = language?.lowercased().trimmingCharacters(in: .whitespaces) ?? ""
        if lang == "diff" || lang == "patch" { return true }
        // Sniff the first non-empty lines for unified diff structure.
        let lines = code.components(separatedBy: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .prefix(8)
        // Structural markers: file headers or hunk headers. These alone are
        // enough to call it a diff.
        let structural = lines.filter { line in
            line.hasPrefix("diff --git") || line.hasPrefix("--- ")
                || line.hasPrefix("+++ ") || line.hasPrefix("@@ ")
        }.count
        if structural >= 1 { return true }
        // Otherwise require BOTH added and removed lines so bullet lists
        // ("- item") and inline +/- math never false-positive.
        let added = lines.filter { $0.hasPrefix("+") }.count
        let removed = lines.filter { $0.hasPrefix("-") }.count
        return added >= 1 && removed >= 1
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header

            if isDiff {
                diffBody
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(SyntaxHighlighter.tokenize(code: code, language: language))
                        .font(.system(size: 13, design: .monospaced))
                        .textSelection(.enabled)
                        .padding(.horizontal, Design.Spacing.sm)
                        .padding(.vertical, Design.Spacing.xs)
                }
            }
        }
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Code block" + (language.map { ", \($0)" } ?? ""))
        .accessibilityAction(named: "Copy code") { copyToClipboard() }
    }

    private var header: some View {
        HStack(spacing: Design.Spacing.xs) {
            if let language, !language.isEmpty {
                Text(language)
                    .brandEyebrow()
            }

            Spacer()

            Button(action: copyToClipboard) {
                HStack(spacing: Design.Spacing.xxxs) {
                    Image(systemName: didCopy ? "checkmark" : "doc.on.doc")
                        .font(.system(size: 10, weight: .medium))
                    Text(didCopy ? "Copied" : "Copy")
                        .font(Design.Typography.eyebrow)
                        .textCase(.uppercase)
                        .tracking(1.2)
                }
                .foregroundStyle(didCopy ? Design.Colors.success : Design.Colors.secondaryForeground)
                .animation(Design.Motion.quickResponse, value: didCopy)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.top, Design.Spacing.xs)
    }

    // MARK: - Diff rendering

    private var diffBody: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(diffLines().enumerated()), id: \.offset) { _, line in
                    diffLineView(line)
                }
            }
            .padding(.vertical, Design.Spacing.xxs)
        }
    }

    private struct DiffRenderLine {
        let text: String
        let prefix: String
        let prefixColor: Color
        let textColor: Color
        let background: Color
    }

    private func diffLines() -> [DiffRenderLine] {
        code.components(separatedBy: "\n").map { raw in
            let line = raw.isEmpty ? "" : raw
            if line.hasPrefix("diff --git") || line.hasPrefix("index ")
                || line.hasPrefix("new file") || line.hasPrefix("deleted file")
                || line.hasPrefix("similarity index") || line.hasPrefix("rename from")
                || line.hasPrefix("rename to") || line.hasPrefix("Binary files") {
                return DiffRenderLine(
                    text: line,
                    prefix: "",
                    prefixColor: Design.Colors.tertiaryForeground,
                    textColor: Design.Colors.tertiaryForeground,
                    background: .clear
                )
            }
            if line.hasPrefix("--- ") || line.hasPrefix("+++ ") {
                return DiffRenderLine(
                    text: line,
                    prefix: String(line.prefix(3)).trimmingCharacters(in: .whitespaces),
                    prefixColor: Design.Colors.secondaryForeground,
                    textColor: Design.Colors.secondaryForeground,
                    background: Design.Colors.surface2.opacity(0.35)
                )
            }
            if line.hasPrefix("@@") {
                return DiffRenderLine(
                    text: line,
                    prefix: "",
                    prefixColor: Design.Brand.primary,
                    textColor: Design.Colors.secondaryForeground,
                    background: Design.Brand.primary.opacity(0.12)
                )
            }
            if line.hasPrefix("+") {
                return DiffRenderLine(
                    text: String(line.dropFirst()),
                    prefix: "+",
                    prefixColor: Design.Colors.success,
                    textColor: Design.Colors.foreground,
                    background: Design.Colors.success.opacity(0.14)
                )
            }
            if line.hasPrefix("-") {
                return DiffRenderLine(
                    text: String(line.dropFirst()),
                    prefix: "-",
                    prefixColor: Design.Colors.danger,
                    textColor: Design.Colors.foreground,
                    background: Design.Colors.danger.opacity(0.14)
                )
            }
            return DiffRenderLine(
                text: line.hasPrefix(" ") ? String(line.dropFirst()) : line,
                prefix: " ",
                prefixColor: Design.Colors.tertiaryForeground.opacity(0.4),
                textColor: Design.Colors.secondaryForeground,
                background: .clear
            )
        }
    }

    private func diffLineView(_ line: DiffRenderLine) -> some View {
        HStack(spacing: 0) {
            Text(line.prefix)
                .font(.system(size: 13, design: .monospaced))
                .foregroundStyle(line.prefixColor)
                .frame(width: 14, alignment: .center)

            Text(line.text.isEmpty ? " " : line.text)
                .font(.system(size: 13, design: .monospaced))
                .foregroundStyle(line.textColor)
                .textSelection(.enabled)
                .lineLimit(nil)
        }
        .padding(.vertical, 0.5)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(line.background)
    }

    // MARK: - Copy

    private func copyToClipboard() {
        UIPasteboard.general.string = code
        didCopy = true
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(2))
            didCopy = false
        }
    }
}
