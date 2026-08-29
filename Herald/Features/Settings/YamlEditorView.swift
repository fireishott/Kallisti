import SwiftUI
import UIKit

// MARK: - Shared Controller

/// Build 128.56: thin shared controller so the toolbar (SwiftUI) can drive
/// editing tools on the editor's text view. The representable binds its
/// UITextView to this controller; toolbar buttons call `indentSelection()`,
/// `outdentSelection()` and `toggleCommentSelection()`.
final class YamlEditorController {
    /// The live text view, installed by the representable.
    weak var textView: UITextView?
    /// Called with the updated plain text after any tool application.
    var onTextChange: ((String) -> Void)?

    init() {}

    /// Insert two spaces at the start of every line in (or touching) the
    /// selection. With an empty selection, indents the current line.
    func indentSelection() {
        guard let textView else { return }
        let result = transformSelectedLines(textView) { line in
            "  " + line
        }
        apply(textView, result)
    }

    /// Remove up to two leading spaces from every line in the selection.
    func outdentSelection() {
        guard let textView else { return }
        let result = transformSelectedLines(textView) { line in
            let leadingCount = line.prefix(while: { $0 == " " }).count
            return String(line.dropFirst(min(2, leadingCount)))
        }
        apply(textView, result)
    }

    /// Toggle a leading `# ` comment marker on every line in the selection.
    /// The marker sits after any indentation, so `  foo:` alternates with
    /// `  # foo:` and YAML structure stays intact.
    func toggleCommentSelection() {
        guard let textView else { return }
        let result = transformSelectedLines(textView) { line in
            let leading = line.prefix(while: { $0 == " " || $0 == "\t" })
            let rest = String(line.dropFirst(leading.count))
            if rest.hasPrefix("# ") {
                return String(leading) + String(rest.dropFirst(2))
            } else if rest.hasPrefix("#") {
                return String(leading) + String(rest.dropFirst(1))
            } else {
                return String(leading) + "# " + rest
            }
        }
        apply(textView, result)
    }

    // MARK: - Find (Build 135.32)

    /// Find the next occurrence of `query` from the current selection and scroll to it.
    /// Returns true if found.
    @discardableResult
    func findNext(_ query: String) -> Bool {
        guard let textView, !query.isEmpty else { return false }
        let text = textView.text ?? ""
        let nsText = text as NSString
        let start = NSMaxRange(textView.selectedRange)
        let searchRange = NSRange(location: start, length: nsText.length - start)
        let range = nsText.range(of: query, options: [], range: searchRange)
        if range.location != NSNotFound {
            textView.selectedRange = range
            textView.scrollRangeToVisible(range)
            return true
        }
        // Wrap around from beginning
        let wrapRange = NSRange(location: 0, length: min(start, nsText.length))
        let wrap = nsText.range(of: query, options: [], range: wrapRange)
        if wrap.location != NSNotFound {
            textView.selectedRange = wrap
            textView.scrollRangeToVisible(wrap)
            return true
        }
        return false
    }

    /// Find the previous occurrence of `query` from the current selection.
    @discardableResult
    func findPrevious(_ query: String) -> Bool {
        guard let textView, !query.isEmpty else { return false }
        let text = textView.text ?? ""
        let nsText = text as NSString
        let end = textView.selectedRange.location
        let searchRange = NSRange(location: 0, length: min(end, nsText.length))
        let range = nsText.range(of: query, options: .backwards, range: searchRange)
        if range.location != NSNotFound {
            textView.selectedRange = range
            textView.scrollRangeToVisible(range)
            return true
        }
        // Wrap around from end
        let wrapRange = NSRange(location: min(end, nsText.length), length: nsText.length - min(end, nsText.length))
        let wrap = nsText.range(of: query, options: .backwards, range: wrapRange)
        if wrap.location != NSNotFound {
            textView.selectedRange = wrap
            textView.scrollRangeToVisible(wrap)
            return true
        }
        return false
    }

    // MARK: - Engine

    /// Expand the selection to whole lines, run `transform` on each touched
    /// line, and return the rebuilt text plus the union line range to
    /// re-select afterwards.
    private func transformSelectedLines(
        _ textView: UITextView,
        transform: (String) -> String
    ) -> (text: String, selection: NSRange) {
        let text = textView.text ?? ""
        let nsText = text as NSString
        let range = textView.selectedRange

        // Clamp the selection to the document.
        let loc = min(max(range.location, 0), nsText.length)
        let len = min(max(range.length, 0), nsText.length - loc)

        // Line starts: index of every line's first character.
        var lineStarts: [Int] = [0]
        for (i, ch) in text.enumerated() where ch == "\n" {
            lineStarts.append(i + 1)
        }
        let lineCount = lineStarts.count

        func lineIndex(atChar pos: Int) -> Int {
            guard lineCount > 1 else { return 0 }
            var lo = 0, hi = lineCount - 1
            while lo < hi {
                let mid = (lo + hi + 1) / 2
                if lineStarts[mid] <= pos { lo = mid } else { hi = mid - 1 }
            }
            return lo
        }

        let firstChar = min(loc, max(0, nsText.length - 1))
        let lastChar = min(max(loc + len, loc), max(0, nsText.length - 1))
        let firstLine = lineIndex(atChar: firstChar)
        let lastLine = lineIndex(atChar: lastChar)

        // Split into physical lines (keeps trailing newline semantics: an
        // "a\nb\n" document splits to ["a","b",""] and joins back unchanged).
        let lines = text.components(separatedBy: "\n")
        var newLines = lines
        for i in firstLine...min(lastLine, newLines.count - 1) {
            newLines[i] = transform(newLines[i])
        }

        let newText = newLines.joined(separator: "\n")

        // Re-select from the start of the first touched line to the end of the
        // last touched line (including its trailing newline, when present).
        var newLineStarts: [Int] = [0]
        for (i, ch) in newText.enumerated() where ch == "\n" { newLineStarts.append(i + 1) }
        let newFirst = newLineStarts[min(firstLine, newLineStarts.count - 1)]

        var newEnd = newFirst
        for _ in firstLine...lastLine {
            if newEnd < newText.count {
                let tail = newText[newText.index(newText.startIndex, offsetBy: newEnd)...]
                if let nextNewline = tail.firstIndex(of: "\n") {
                    newEnd += newText.distance(from: tail.startIndex, to: nextNewline) + 1
                } else {
                    newEnd = newText.count
                }
            }
        }

        return (newText, NSRange(location: newFirst, length: newEnd - newFirst))
    }

    private func apply(_ textView: UITextView, _ result: (text: String, selection: NSRange)) {
        guard textView.text != result.text else {
            onTextChange?(textView.text)
            return
        }
        textView.text = result.text
        if result.selection.location <= (textView.text as NSString).length {
            textView.selectedRange = result.selection
        }
        onTextChange?(result.text)
    }
}

// MARK: - Representable

/// Build 128.56: line-numbered UITextView editor with YAML syntax
/// highlighting. Gutter and text share the same monospaced font metrics and
/// scroll in lockstep, so line numbers always align with content.
struct YamlEditorView: UIViewRepresentable {
    @Binding var text: String
    /// Optional shared controller - when provided, the representable installs
    /// its text view on the controller so external tools can drive it.
    var controller: YamlEditorController?

    static let editorFont: UIFont = .monospacedSystemFont(ofSize: 13, weight: .regular)
    static let gutterWidth: CGFloat = 46
    static let editorLineSpacing: CGFloat = 2
    static var lineHeight: CGFloat { editorFont.lineHeight + editorLineSpacing }

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> LineNumberedTextView {
        let container = LineNumberedTextView(gutterWidth: Self.gutterWidth)

        let textView = container.textView
        textView.delegate = context.coordinator
        textView.font = Self.editorFont
        textView.text = text
        textView.backgroundColor = .clear
        textView.textColor = UIColor(Design.Colors.foreground)
        textView.keyboardAppearance = .dark
        textView.autocapitalizationType = .none
        textView.autocorrectionType = .no
        textView.smartQuotesType = .no
        textView.smartDashesType = .no
        textView.textContainerInset = UIEdgeInsets(
            top: Design.Spacing.sm,
            left: Design.Spacing.sm,
            bottom: Design.Spacing.sm,
            right: Design.Spacing.sm
        )
        textView.textContainer.lineFragmentPadding = 0
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineSpacing = Self.editorLineSpacing
        textView.typingAttributes = [
            .font: Self.editorFont,
            .foregroundColor: UIColor(Design.Colors.foreground),
            .paragraphStyle: paragraph,
        ]

        // Gutter styling
        let gutter = container.gutterLabel
        gutter.font = Self.editorFont
        gutter.textColor = UIColor(Design.Colors.tertiaryForeground)
        gutter.textAlignment = .right

        // Wire the binding update through the coordinator. Capturing the
        // Binding (not the struct) is the safe SwiftUI pattern.
        let binding = $text
        context.coordinator.onTextChange = { newText in
            binding.wrappedValue = newText
        }

        // Install the shared controller on this instance.
        controller?.textView = textView
        controller?.onTextChange = { [weak coordinator = context.coordinator] plain in
            coordinator?.onTextChange?(plain)
            // Tools set text programmatically, which skips the delegate, so
            // re-apply styling + gutter here to keep the editor consistent.
            coordinator?.restyle(plain, textView: textView)
        }

        // Initial content
        context.coordinator.refresh(with: text, textView: textView)
        return container
    }

    func updateUIView(_ uiView: LineNumberedTextView, context: Context) {
        let textView = uiView.textView
        if textView.text != text {
            let selection = textView.selectedRange
            context.coordinator.refresh(with: text, textView: textView)
            if selection.location <= (textView.text as NSString).length {
                textView.selectedRange = selection
            }
        }
    }

    // MARK: - Coordinator

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: YamlEditorView
        /// Forwards edited text back to the SwiftUI binding.
        var onTextChange: ((String) -> Void)?

        init(_ parent: YamlEditorView) {
            self.parent = parent
        }

        /// Rebuild gutter numbers + highlighting. Called on init and whenever
        /// the SwiftUI binding pushes a different string (e.g. load / save).
        func refresh(with plainText: String, textView: UITextView) {
            let attributed = YamlEditorView.highlight(plainText, font: YamlEditorView.editorFont)
            textView.attributedText = attributed
            updateGutter(for: plainText, textView: textView)
        }

        /// Re-style after programmatic edits (controller tools), preserving
        /// the current caret/selection.
        func restyle(_ plain: String, textView: UITextView) {
            let selection = textView.selectedRange
            let attributed = YamlEditorView.highlight(plain, font: YamlEditorView.editorFont)
            textView.attributedText = attributed
            if selection.location <= (textView.text as NSString).length {
                textView.selectedRange = selection
            }
            updateGutter(for: plain, textView: textView)
        }

        // MARK: UITextViewDelegate

        func textViewDidChange(_ textView: UITextView) {
            let plain = textView.text ?? ""
            restyle(plain, textView: textView)
            onTextChange?(plain)
        }

        func scrollViewDidScroll(_ scrollView: UIScrollView) {
            // The editor text view is the only scroll view wired to this
            // delegate. Regenerate the visible gutter on every scroll frame -
            // this self-corrects any drift instead of relying on the label
            // staying in perfect sync with the text view's content.
            guard let tv = scrollView as? UITextView else { return }
            guard let container = scrollView.superview as? LineNumberedTextView else { return }
            container.gutterScroll.contentOffset = CGPoint(x: 0, y: tv.contentOffset.y)
            updateVisibleGutter(for: tv.text ?? "", textView: tv)
        }

        /// Build the gutter for the lines currently visible in the viewport.
        /// Numbers are absolute document line numbers, positioned so line N's
        /// number sits exactly beside line N's text. Regenerating from the
        /// scroll position every frame means the gutter can never drift or
        /// freeze at the top of the document.
        private func updateVisibleGutter(for text: String, textView: UITextView) {
            guard let container = textView.superview as? LineNumberedTextView else { return }

            let lineHeight = YamlEditorView.lineHeight
            let insetTop = textView.textContainerInset.top
            let totalLineCount = max(1, text.split(separator: "\n", omittingEmptySubsequences: false).count)

            // Which document line sits at (or above) the viewport's top edge?
            let contentTop = max(0, textView.contentOffset.y)
            var firstVisible = Int(floor((contentTop - insetTop) / lineHeight))
            firstVisible = max(0, min(firstVisible, totalLineCount - 1))

            // How many lines fit in the viewport (+2 line buffer so a fast
            // scroll never briefly shows an empty gutter). Before layout the
            // text view bounds may be zero, so fall back to the container
            // height for the initial render.
            let viewportHeight = textView.bounds.height > 0 ? textView.bounds.height : container.bounds.height
            let visibleLineCount = max(1, Int(ceil(viewportHeight / lineHeight)) + 2)
            let lastVisible = min(totalLineCount - 1, firstVisible + visibleLineCount)

            // Absolute line numbers for the visible window only.
            let numbers = (firstVisible + 1...lastVisible + 1).map(String.init).joined(separator: "\n")

            // Same paragraph metrics as the text view so label row heights
            // match the editor's line spacing exactly.
            let para = NSMutableParagraphStyle()
            para.lineSpacing = YamlEditorView.editorLineSpacing
            container.gutterLabel.attributedText = NSAttributedString(
                string: numbers,
                attributes: [
                    .font: YamlEditorView.editorFont,
                    .foregroundColor: UIColor(Design.Colors.tertiaryForeground),
                    .paragraphStyle: para,
                ]
            )

            // Position the label at the first visible line's content offset.
            let lineTop = insetTop + CGFloat(firstVisible) * lineHeight
            container.gutterLabel.frame = CGRect(
                x: 0,
                y: lineTop,
                width: YamlEditorView.gutterWidth,
                height: CGFloat(lastVisible - firstVisible + 1) * lineHeight
            )
            container.gutterScroll.contentSize = CGSize(
                width: YamlEditorView.gutterWidth,
                height: insetTop + CGFloat(totalLineCount) * lineHeight + textView.textContainerInset.bottom
            )
        }

        private func updateGutter(for text: String, textView: UITextView) {
            guard let container = textView.superview as? LineNumberedTextView else { return }
            let lineCount = max(1, text.split(separator: "\n", omittingEmptySubsequences: false).count)

            let insetTop = textView.textContainerInset.top
            let contentHeight = insetTop + CGFloat(lineCount) * YamlEditorView.lineHeight + textView.textContainerInset.bottom

            // Sync the gutter scroll position with the text view so the
            // visible window aligns with wherever the user is scrolled.
            container.gutterScroll.contentOffset = CGPoint(x: 0, y: textView.contentOffset.y)
            container.gutterScroll.contentSize = CGSize(
                width: YamlEditorView.gutterWidth,
                height: contentHeight
            )
            updateVisibleGutter(for: text, textView: textView)
        }
    }
}

// MARK: - Highlighting

extension YamlEditorView {
    /// Lightweight, dependency-free YAML highlighting. Scalar values, keys,
    /// strings, numbers/booleans and comments get distinct colors; everything
    /// else stays primary foreground. Runs on every text change - cost is
    /// linear in the (small) config document.
    ///
    /// Order matters: structures paint first, comments paint LAST so a `#`
    /// line with a colon or number never gets key/number colors bleeding
    /// through.
    static func highlight(_ plain: String, font: UIFont) -> NSAttributedString {
        let attributed = NSMutableAttributedString(string: plain)
        let fullRange = NSRange(location: 0, length: (plain as NSString).length)

        let paragraph = NSMutableParagraphStyle()
        paragraph.lineSpacing = editorLineSpacing

        attributed.addAttributes([
            .font: font,
            .foregroundColor: UIColor(Design.Colors.foreground),
            .paragraphStyle: paragraph,
        ], range: fullRange)

        // Strings: 'single' or "double" quoted segments.
        let stringRegex = try? NSRegularExpression(pattern: #"(['"])(?:\\.|(?!\1).)*\1"#)
        stringRegex?.enumerateMatches(in: plain, options: [], range: fullRange) { match, _, _ in
            guard let match else { return }
            attributed.addAttribute(.foregroundColor, value: UIColor(Design.Colors.success), range: match.range)
        }

        // Numbers: integers, decimals, exponents.
        let numberRegex = try? NSRegularExpression(pattern: #"(?<![-\w])(-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)(?![\w.])"#)
        numberRegex?.enumerateMatches(in: plain, options: [], range: fullRange) { match, _, _ in
            guard let match else { return }
            attributed.addAttribute(.foregroundColor, value: UIColor(Design.Colors.warning), range: match.range)
        }

        // Booleans / null / common YAML scalars.
        let boolRegex = try? NSRegularExpression(pattern: #"\b(true|false|null|yes|no|on|off|~)\b"#, options: [.caseInsensitive])
        boolRegex?.enumerateMatches(in: plain, options: [], range: fullRange) { match, _, _ in
            guard let match else { return }
            attributed.addAttribute(.foregroundColor, value: UIColor(Design.Colors.warning), range: match.range)
        }

        // Keys: `key:` (or `- key:` / `"key":`) at the start of a line.
        let keyRegex = try? NSRegularExpression(pattern: #"(?m)^([ \t-]*)([^:#\n]+?)(\s*):"#)
        keyRegex?.enumerateMatches(in: plain, options: [], range: fullRange) { match, _, _ in
            guard let match, match.numberOfRanges > 2 else { return }
            let keyRange = match.range(at: 2)
            guard keyRange.length > 0 else { return }
            attributed.addAttribute(.foregroundColor, value: UIColor(Design.Colors.accent), range: keyRange)
        }

        // Comments: # to end of line. LAST so comments always win.
        let commentRegex = try? NSRegularExpression(pattern: #"#[^\n]*"#)
        commentRegex?.enumerateMatches(in: plain, options: [], range: fullRange) { match, _, _ in
            guard let match else { return }
            attributed.addAttribute(.foregroundColor, value: UIColor(Design.Colors.tertiaryForeground), range: match.range)
        }

        return attributed
    }
}

// MARK: - Container View

/// Horizontal container: fixed-width line-number gutter + editable text view.
/// The gutter scroll view scrolls in lockstep with the text view's own scroll.
final class LineNumberedTextView: UIView {
    let gutterScroll = UIScrollView()
    let gutterLabel = UILabel()
    let textView = UITextView()

    private let gutterWidth: CGFloat

    init(gutterWidth: CGFloat = 46) {
        self.gutterWidth = gutterWidth
        super.init(frame: .zero)

        backgroundColor = .clear

        gutterScroll.isScrollEnabled = false
        gutterScroll.showsVerticalScrollIndicator = false
        gutterScroll.showsHorizontalScrollIndicator = false
        gutterScroll.backgroundColor = .clear

        gutterLabel.numberOfLines = 0
        gutterLabel.lineBreakMode = .byCharWrapping

        addSubview(gutterScroll)
        gutterScroll.addSubview(gutterLabel)
        addSubview(textView)

        gutterScroll.translatesAutoresizingMaskIntoConstraints = false
        textView.translatesAutoresizingMaskIntoConstraints = false

        NSLayoutConstraint.activate([
            gutterScroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            gutterScroll.topAnchor.constraint(equalTo: topAnchor),
            gutterScroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            gutterScroll.widthAnchor.constraint(equalToConstant: gutterWidth),

            textView.leadingAnchor.constraint(equalTo: gutterScroll.trailingAnchor),
            textView.trailingAnchor.constraint(equalTo: trailingAnchor),
            textView.topAnchor.constraint(equalTo: topAnchor),
            textView.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])

        // Keep the gutter visually separated with a hairline.
        let separator = UIView()
        separator.backgroundColor = UIColor(Design.Colors.divider)
        separator.translatesAutoresizingMaskIntoConstraints = false
        addSubview(separator)
        NSLayoutConstraint.activate([
            separator.leadingAnchor.constraint(equalTo: gutterScroll.trailingAnchor),
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomAnchor),
            separator.widthAnchor.constraint(equalToConstant: 0.5),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }
}