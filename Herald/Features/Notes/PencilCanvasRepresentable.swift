import PencilKit
import SwiftUI

/// UIViewRepresentable wrapper for PKCanvasView.
/// The coordinator owns the canvas, delegate, tool picker observation, and paper layer.
/// The canvas must NOT be recreated on SwiftUI state refresh — assert identity.
struct PencilCanvasRepresentable: UIViewRepresentable {
    @Binding var drawing: PKDrawing
    var pageStyle: NotePageStyle
    var pencilOnly: Bool = true
    /// Explicit column width from the SwiftUI layout (GeometryReader).
    /// Without this, a sidebar drag changes the frame but SwiftUI never
    /// re-invokes updateUIView (no state changed), so the canvas keeps
    /// its old width. Passing the width as a tracked property forces the
    /// update pass and the canvas follows the sidebar.
    var canvasWidth: CGFloat? = nil
    /// Build 130.2: user line-height override in points. 0 = follow the
    /// paper style's default spacing; >0 forces the actual distance
    /// between ruled lines.
    var lineSpacing: Double = 0
    var onDrawingChanged: ((PKDrawing) -> Void)?
    var onToolUseBegan: (() -> Void)?
    var onToolUseEnded: (() -> Void)?
    /// Notes fix: when the canvas view scrolls or zooms, surface the new
    /// viewport so the editor can persist it with the note and restore
    /// it on reopen.
    var onViewportChanged: ((CanvasViewport) -> Void)?
    /// Notes fix: when the canvas re-mounts, the caller hands back the
    /// last-known viewport. The coordinator applies it after the first
    /// layout settles so we don't fight the PencilKit zoomScale animation.
    var initialViewport: CanvasViewport?

    /// Notes fix: portable description of the PencilKit viewport. Saved
    /// alongside the note and restored on reopen so the editor lands at
    /// the same scroll position + zoom the user last left.
    struct CanvasViewport: Equatable, Codable, Sendable {
        var contentOffsetX: Double
        var contentOffsetY: Double
        var zoomScale: Double
        var contentWidth: Double
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> PKCanvasView {
        let canvas = PKCanvasView()
        canvas.delegate = context.coordinator
        canvas.drawing = drawing
        canvas.drawingPolicy = pencilOnly ? .pencilOnly : .anyInput
        canvas.backgroundColor = .clear
        canvas.isOpaque = false
        canvas.maximumZoomScale = 4.0
        canvas.minimumZoomScale = 1.0
        
        // Enable infinite vertical scrolling like Apple Notes
        canvas.contentSize = CGSize(width: canvas.bounds.width, height: 4000)
        canvas.alwaysBounceVertical = true
        canvas.showsVerticalScrollIndicator = true
        
        context.coordinator.canvasView = canvas

        // Install paper layer behind canvas content
        context.coordinator.installPaper(in: canvas, style: pageStyle)

        // Tool picker — instance-based API (iOS 16+)
        let picker = PKToolPicker()
        picker.setVisible(true, forFirstResponder: canvas)
        picker.addObserver(canvas)
        picker.stateAutosaveName = "kallisti.canvas"
        context.coordinator.toolPicker = picker

        // Pencil interactions — honor system preferred actions for double-tap and squeeze
        let pencilInteraction = UIPencilInteraction()
        pencilInteraction.delegate = context.coordinator
        canvas.addInteraction(pencilInteraction)

        // CRITICAL: Block parent ScrollView from intercepting pencil touches
        canvas.panGestureRecognizer.require(toFail: canvas.drawingGestureRecognizer)

        // Monitor scroll position to auto-extend canvas
        context.coordinator.installAutoGrow(canvas: canvas)

        canvas.becomeFirstResponder()

        // Accessibility
        canvas.accessibilityLabel = "Drawing canvas"
        canvas.accessibilityHint = "Use Apple Pencil or finger to draw. Double-tap or squeeze for tool switching."

        return canvas
    }

    func updateUIView(_ canvas: PKCanvasView, context: Context) {
        // Only update if the drawing changed externally (e.g., loaded from disk)
        // Never overwrite during active drawing — the coordinator handles that.
        if drawing != canvas.drawing && !context.coordinator.isDrawingActive {
            canvas.drawing = drawing
        }

        // Update drawing policy if pencilOnly changed
        let desiredPolicy: PKCanvasViewDrawingPolicy = pencilOnly ? .pencilOnly : .anyInput
        if canvas.drawingPolicy != desiredPolicy {
            canvas.drawingPolicy = desiredPolicy
        }

        // Update paper if style changed
        if context.coordinator.currentStyle != pageStyle {
            context.coordinator.currentStyle = pageStyle
            context.coordinator.updatePaper(style: pageStyle)
        }

        // Build 130.2: update paper if the user's line-height override
        // changed. A separate tracked value so moving the slider repaints
        // the ruled lines immediately, even when the style is unchanged.
        if context.coordinator.currentLineSpacing != lineSpacing {
            context.coordinator.currentLineSpacing = lineSpacing
            context.coordinator.updatePaper(style: context.coordinator.currentStyle)
        }

        // Keep canvas width in sync with the view frame so lines span the
        // full column width (matches iOS Notes behavior).  Only height
        // auto-grows; width is driven by the SwiftUI layout. When an explicit
        // width is tracked (sidebar drag case), prefer it over bounds so the
        // resize actually happens even if SwiftUI doesn't relayout bounds.
        //
        // Keep layout resizing separate from the user's viewport. PencilKit owns
        // zoomScale and contentOffset while fingers are on the canvas.
        let targetWidth = canvasWidth ?? canvas.bounds.width
        if targetWidth > 0, canvas.contentSize.width != targetWidth {
            canvas.contentSize.width = targetWidth
            context.coordinator.updatePaper(style: context.coordinator.currentStyle)
        }

        if !context.coordinator.didApplyInitialViewport, let viewport = initialViewport, canvas.bounds.width > 0 {
            canvas.minimumZoomScale = min(canvas.minimumZoomScale, CGFloat(viewport.zoomScale))
            canvas.zoomScale = min(max(CGFloat(viewport.zoomScale), canvas.minimumZoomScale), canvas.maximumZoomScale)
            canvas.contentOffset = CGPoint(x: CGFloat(viewport.contentOffsetX), y: CGFloat(viewport.contentOffsetY))
            context.coordinator.didApplyInitialViewport = true
        }

        // Restore tool picker and first responder after sheet/rotation/backgrounding.
        // Only when the canvas is actually on screen - never resurrect the picker
        // for an off-screen or removed view (that is how the floating markup bar
        // leaks onto other screens when the note editor closes).
        if canvas.window != nil,
           let picker = context.coordinator.toolPicker {
            if !picker.isVisible {
                picker.setVisible(true, forFirstResponder: canvas)
            }
            if !canvas.isFirstResponder {
                canvas.becomeFirstResponder()
            }
        }
    }

    /// UIKit teardown - called by SwiftUI when this view is removed from the hierarchy.
    /// This is the missing half of the tool-picker lifecycle. Without it the
    /// PKToolPicker (a system floating window) stays visible forever, overlapping
    /// the composer and other chrome after leaving the note editor.
    static func dismantleUIView(_ uiView: PKCanvasView, coordinator: Coordinator) {
        coordinator.toolPicker?.setVisible(false, forFirstResponder: uiView)
        coordinator.toolPicker?.removeObserver(uiView)
        coordinator.toolPicker = nil
        if uiView.isFirstResponder {
            uiView.resignFirstResponder()
        }
        coordinator.canvasView = nil
    }

    // MARK: - Coordinator

    final class Coordinator: NSObject, PKCanvasViewDelegate, UIPencilInteractionDelegate {
        var parent: PencilCanvasRepresentable
        var toolPicker: PKToolPicker?
        weak var canvasView: PKCanvasView?
        var isDrawingActive = false
        var currentStyle: NotePageStyle
        /// Build 130.2: last-applied user line-height override (points).
        /// Tracks changes so the paper layer repaints on slider moves.
        var currentLineSpacing: Double = 0
        private weak var paperView: NotePaperUIView?
        /// Notes fix: last viewport we emitted to the parent - used to
        /// dedupe rapid updates from the scroll-view observer.
        var lastEmittedViewport: PencilCanvasRepresentable.CanvasViewport?
        /// Notes fix: whether we've applied the editor's initial viewport
        /// on this representable instance. Prevents fighting PencilKit.
        var didApplyInitialViewport: Bool = false
        private var contentObserver: NSKeyValueObservation?
        private var scrollObserver: NSKeyValueObservation?
        /// Build 132.2: observes the canvas's own bounds so the paper width
        /// self-heals on rotation/sidebar/split-view layout changes that
        /// SwiftUI's updateUIView cycle misses.
        private var boundsObserver: NSKeyValueObservation?

        init(_ parent: PencilCanvasRepresentable) {
            self.parent = parent
            self.currentStyle = parent.pageStyle
            self.currentLineSpacing = parent.lineSpacing
        }

        func installAutoGrow(canvas: PKCanvasView) {
            scrollObserver = canvas.observe(\.contentOffset, options: [.new]) { [weak self] scrollView, _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    let bottomEdge = scrollView.contentOffset.y + scrollView.bounds.height
                    let contentHeight = scrollView.contentSize.height
                    // Grow only when the user reaches the bottom. Do not feed each
                    // scroll tick back into SwiftUI while PencilKit is drawing.
                    if bottomEdge > contentHeight - 200 {
                        scrollView.contentSize.height = contentHeight + 2000
                        self.updatePaper(style: self.currentStyle)
                    }
                }
            }
        }

        /// The canvas moves many times per stroke. Persist only settled viewport
        /// positions, never every contentOffset notification.
        private func emitSettledViewport(from canvas: PKCanvasView) {
            let viewport = PencilCanvasRepresentable.CanvasViewport(
                contentOffsetX: canvas.contentOffset.x,
                contentOffsetY: canvas.contentOffset.y,
                zoomScale: canvas.zoomScale,
                contentWidth: canvas.contentSize.width
            )
            guard lastEmittedViewport != viewport else { return }
            lastEmittedViewport = viewport
            parent.onViewportChanged?(viewport)
        }

        func installPaper(in canvas: PKCanvasView, style: NotePageStyle) {
            let paper = NotePaperUIView(style: style, lineSpacing: parent.lineSpacing, frame: canvas.bounds)
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
                    // Build 133.4: paper lives in the same scroll-view content
                    // coordinate space as contentSize (both scale together with
                    // zoomScale), so the paper frame width is contentSize.width
                    // directly - NOT divided by zoom (that double-scaled it and
                    // made the ruled lines overrun the column once auto-fit
                    // zoomed below 1.0).
                    let paperSize = CGSize(
                        width: contentSize.width,
                        height: contentSize.height
                    )
                    if paper.frame.size != paperSize {
                        paper.frame = CGRect(origin: .zero, size: paperSize)
                        paper.setNeedsDisplay()
                    }
                }
            }

            // Build 132.2: self-heal the paper width on ANY layout change
            // (rotation, sidebar close, split-view resize). SwiftUI's
            // updateUIView only re-fires when the representable's inputs
            // change - the GeometryReader proxy width alone doesn't always
            // trigger it, so the canvas keeps a stale narrow width and the
            // ruled lines stop partway across the screen in portrait. Rotation
            // happened to force a relayout, which is why lines "fixed"
            // themselves after rotating to landscape. Observing the canvas's
            // own bounds closes the loop: whenever the visible frame changes,
            // sync contentSize.width + the paper frame so lines span edge to
            // edge without waiting on SwiftUI's update cycle.
            boundsObserver = canvas.observe(\.bounds, options: [.new, .initial]) { [weak self] scrollView, _ in
                MainActor.assumeIsolated { [weak self] in
                    guard let self else { return }
                    let width = scrollView.bounds.width
                    guard width > 0 else { return }
                    let target = self.parent.canvasWidth ?? width
                    if scrollView.contentSize.width != target {
                        scrollView.contentSize.width = target
                    }
                    // Force the paper to match the canvas content width so the
                    // ruled lines draw edge to edge. The contentSize KVO above
                    // normally handles this, but only fires on CHANGE - if the
                    // width didn't actually change (stale contentSize equals
                    // new width by coincidence) the paper never repaints.
                    if let paper = self.paperView {
                        // Paper spans the content width in scroll coords (scales
                        // with zoom automatically) - see contentObserver note.
                        let paperWidth = scrollView.contentSize.width
                        if paper.bounds.width != paperWidth {
                            paper.frame = CGRect(origin: .zero, size: CGSize(width: paperWidth, height: paper.bounds.height))
                        }
                        paper.setNeedsDisplay()
                    }
                }
            }
        }

        func updatePaper(style: NotePageStyle) {
            paperView?.style = style
            paperView?.lineSpacing = parent.lineSpacing
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
            emitSettledViewport(from: canvasView)
        }

        func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate decelerate: Bool) {
            if !decelerate, let canvas = scrollView as? PKCanvasView {
                emitSettledViewport(from: canvas)
            }
        }

        func scrollViewDidEndDecelerating(_ scrollView: UIScrollView) {
            if let canvas = scrollView as? PKCanvasView {
                emitSettledViewport(from: canvas)
            }
        }

        func scrollViewDidEndZooming(_ scrollView: UIScrollView, with view: UIView?, atScale scale: CGFloat) {
            if let canvas = scrollView as? PKCanvasView {
                emitSettledViewport(from: canvas)
            }
        }

        // MARK: - UIPencilInteractionDelegate

        func pencilInteractionDidTap(_ interaction: UIPencilInteraction) {
            // Honor the user's system-preferred double-tap action (Settings > Apple Pencil)
            switch UIPencilInteraction.preferredTapAction {
            case .switchEraser:
                toggleEraser()
            case .switchPrevious:
                switchToPreviousTool()
            case .showColorPalette:
                showToolPicker()
            case .showInkAttributes:
                showToolPicker()
            case .showContextualPalette:
                showToolPicker()
            case .runSystemShortcut:
                break
            case .ignore:
                break
            @unknown default:
                break
            }
        }

        @available(iOS 17.5, *)
        func pencilInteraction(_ interaction: UIPencilInteraction, didReceiveSqueeze squeeze: UIPencilInteraction.Squeeze) {
            // Honor the user's system-preferred squeeze action (Apple Pencil Pro)
            if squeeze.phase == .ended {
                switch UIPencilInteraction.preferredSqueezeAction {
                case .switchEraser:
                    toggleEraser()
                case .switchPrevious:
                    switchToPreviousTool()
                case .showColorPalette:
                    showToolPicker()
                case .showInkAttributes:
                    showToolPicker()
                case .showContextualPalette:
                    showToolPicker()
                case .runSystemShortcut:
                    break
                case .ignore:
                    break
                @unknown default:
                    break
                }
            }
        }

        private func toggleEraser() {
            guard let canvas = canvasView else { return }
            if canvas.tool is PKEraserTool {
                canvas.tool = PKInkingTool(.pen, color: .black, width: 2)
            } else {
                canvas.tool = PKEraserTool(.vector)
            }
        }

        private func switchToPreviousTool() {
            guard let canvas = canvasView else { return }
            if canvas.tool is PKEraserTool {
                canvas.tool = PKInkingTool(.pen, color: .black, width: 2)
            } else {
                canvas.tool = PKEraserTool(.vector)
            }
        }

        private func showToolPicker() {
            guard let picker = toolPicker, let canvas = canvasView else { return }
            picker.setVisible(true, forFirstResponder: canvas)
            canvas.becomeFirstResponder()
        }
    }
}

// MARK: - Paper UIView (Core Graphics rendering for UIKit interop)

/// UIView subclass that draws the note paper pattern using Core Graphics.
/// Used inside PKCanvasView (UIKit) so paper scrolls and zooms with ink.
final class NotePaperUIView: UIView {
    var style: NotePageStyle
    /// Build 130.2: user line-height override in points. 0 = follow the
    /// style's default spacing (fine 20 / medium 24 / wide 32); >0 forces
    /// the actual distance between ruled lines.
    var lineSpacing: Double

    init(style: NotePageStyle, lineSpacing: Double = 0, frame: CGRect) {
        self.style = style
        self.lineSpacing = lineSpacing
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
        // Build 130.2: user override wins when set; otherwise fall back to
        // the paper style's default spacing. This is the actual pixel
        // distance between ruled lines - the thing the Line height slider
        // must visibly change.
        let spacing = lineSpacing > 0 ? CGFloat(lineSpacing) : style.lineSpacing
        guard spacing > 0 else { return }

        let trait = traitCollection.userInterfaceStyle
        let isDark = trait == .dark

        let lineColor: UIColor = isDark
            ? UIColor.white.withAlphaComponent(0.12)
            : UIColor.systemGray3
        let marginColor: UIColor = isDark
            ? UIColor.red.withAlphaComponent(0.12)
            : UIColor.red.withAlphaComponent(0.06)
        let lineWidth: CGFloat = isDark ? 2.0 : 1.5

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
            ctx.setLineWidth(1.5)
            ctx.move(to: CGPoint(x: leftMargin, y: 0))
            ctx.addLine(to: CGPoint(x: leftMargin, y: size.height))
            ctx.strokePath()
        }
    }
}
