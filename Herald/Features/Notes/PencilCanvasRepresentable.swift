import PencilKit
import SwiftUI

/// UIViewRepresentable wrapper for PKCanvasView.
/// The coordinator owns the canvas, delegate, tool picker observation, and paper layer.
/// The canvas must NOT be recreated on SwiftUI state refresh — assert identity.
struct PencilCanvasRepresentable: UIViewRepresentable {
    @Binding var drawing: PKDrawing
    var pageStyle: NotePageStyle
    var onDrawingChanged: ((PKDrawing) -> Void)?
    var onToolUseBegan: (() -> Void)?
    var onToolUseEnded: (() -> Void)?

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> PKCanvasView {
        let canvas = PKCanvasView()
        canvas.delegate = context.coordinator
        canvas.drawing = drawing
        canvas.drawingPolicy = .default
        canvas.backgroundColor = .clear
        canvas.isOpaque = false
        canvas.maximumZoomScale = 4.0
        canvas.minimumZoomScale = 1.0

        // Install paper layer behind canvas content
        context.coordinator.installPaper(in: canvas, style: pageStyle)

        // Tool picker — shared instance, stable autosave name
        if let window = canvas.window ?? UIApplication.shared.connectedScenes
            .compactMap({ $0 as? UIWindowScene })
            .first?.windows.first,
           let picker = PKToolPicker.shared(for: window) {
            picker.setVisible(true, forFirstResponder: canvas)
            picker.addObserver(canvas)
            picker.stateAutosaveName = "herald.canvas"
            context.coordinator.toolPicker = picker
        }

        canvas.becomeFirstResponder()
        return canvas
    }

    func updateUIView(_ canvas: PKCanvasView, context: Context) {
        // Only update if the drawing changed externally (e.g., loaded from disk)
        // Never overwrite during active drawing — the coordinator handles that.
        if drawing != canvas.drawing && !context.coordinator.isDrawingActive {
            canvas.drawing = drawing
        }

        // Update paper if style changed
        if context.coordinator.currentStyle != pageStyle {
            context.coordinator.currentStyle = pageStyle
            context.coordinator.updatePaper(style: pageStyle)
        }
    }

    // MARK: - Coordinator

    final class Coordinator: NSObject, PKCanvasViewDelegate {
        var parent: PencilCanvasRepresentable
        var toolPicker: PKToolPicker?
        var isDrawingActive = false
        var currentStyle: NotePageStyle
        private weak var paperView: NotePaperUIView?
        private var contentObserver: NSKeyValueObservation?

        init(_ parent: PencilCanvasRepresentable) {
            self.parent = parent
            self.currentStyle = parent.pageStyle
        }

        func installPaper(in canvas: PKCanvasView, style: NotePageStyle) {
            let paper = NotePaperUIView(style: style, frame: canvas.bounds)
            paper.backgroundColor = .clear
            paper.isUserInteractionEnabled = false
            paper.autoresizingMask = [.flexibleWidth, .flexibleHeight]

            // Insert at index 0 of the canvas — behind all content
            canvas.insertSubview(paper, at: 0)
            self.paperView = paper

            // Observe contentSize changes to keep paper sized to scroll content.
            // KVO on UIKit properties fires on main thread; use assumeIsolated.
            contentObserver = canvas.observe(\.contentSize, options: [.new]) { [weak self] scrollView, _ in
                MainActor.assumeIsolated { [weak self] in
                    guard let self, let paper = self.paperView else { return }
                    let contentSize = scrollView.contentSize
                    let zoom = scrollView.zoomScale
                    let paperSize = CGSize(
                        width: contentSize.width / max(zoom, 0.01),
                        height: contentSize.height / max(zoom, 0.01)
                    )
                    paper.frame = CGRect(origin: .zero, size: paperSize)
                }
            }
        }

        func updatePaper(style: NotePageStyle) {
            paperView?.style = style
            paperView?.setNeedsDisplay()
        }

        // MARK: - PKCanvasViewDelegate

        func canvasViewDrawingDidChange(_ canvasView: PKCanvasView) {
            parent.drawing = canvasView.drawing
            parent.onDrawingChanged?(canvasView.drawing)
        }

        func canvasViewDidBeginUsingTool(_ canvasView: PKCanvasView) {
            isDrawingActive = true
            parent.onToolUseBegan?()
        }

        func canvasViewDidEndUsingTool(_ canvasView: PKCanvasView) {
            isDrawingActive = false
            parent.onToolUseEnded?()
        }
    }
}

// MARK: - Paper UIView (Core Graphics rendering for UIKit interop)

/// UIView subclass that draws the note paper pattern using Core Graphics.
/// Used inside PKCanvasView (UIKit) so paper scrolls and zooms with ink.
final class NotePaperUIView: UIView {
    var style: NotePageStyle

    init(style: NotePageStyle, frame: CGRect) {
        self.style = style
        super.init(frame: frame)
        isOpaque = false
        backgroundColor = .clear
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    override func draw(_ rect: CGRect) {
        guard let ctx = UIGraphicsGetCurrentContext() else { return }

        let size = bounds.size
        let spacing = style.lineSpacing
        guard spacing > 0 else { return }

        let trait = traitCollection.userInterfaceStyle
        let isDark = trait == .dark

        let lineColor: UIColor = isDark
            ? UIColor.white.withAlphaComponent(0.12)
            : UIColor.systemGray3
        let marginColor: UIColor = isDark
            ? UIColor.red.withAlphaComponent(0.12)
            : UIColor.red.withAlphaComponent(0.06)
        let lineWidth: CGFloat = isDark ? 0.75 : 0.5

        ctx.setStrokeColor(lineColor.cgColor)
        ctx.setLineWidth(lineWidth)

        // Horizontal lines
        if style.showsRuledLines || style.showsGrid {
            var y: CGFloat = spacing
            while y < size.height {
                ctx.move(to: CGPoint(x: 0, y: y))
                ctx.addLine(to: CGPoint(x: size.width, y: y))
                y += spacing
            }
            ctx.strokePath()
        }

        // Vertical lines (grid)
        if style.showsGrid {
            ctx.setStrokeColor(lineColor.cgColor)
            ctx.setLineWidth(lineWidth)
            var x: CGFloat = spacing
            while x < size.width {
                ctx.move(to: CGPoint(x: x, y: 0))
                ctx.addLine(to: CGPoint(x: x, y: size.height))
                x += spacing
            }
            ctx.strokePath()
        }

        // Red margin line
        if style.showsMarginLine {
            let leftMargin: CGFloat = 72
            ctx.setStrokeColor(marginColor.cgColor)
            ctx.setLineWidth(1.0)
            ctx.move(to: CGPoint(x: leftMargin, y: 0))
            ctx.addLine(to: CGPoint(x: leftMargin, y: size.height))
            ctx.strokePath()
        }
    }
}
