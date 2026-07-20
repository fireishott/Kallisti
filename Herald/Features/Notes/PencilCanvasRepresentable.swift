import PencilKit
import SwiftUI

/// UIViewRepresentable wrapper for PKCanvasView.
/// The coordinator owns the canvas, delegate, and tool picker observation.
/// The canvas must NOT be recreated on SwiftUI state refresh — assert identity.
struct PencilCanvasRepresentable: UIViewRepresentable {
    @Binding var drawing: PKDrawing
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
    }

    // MARK: - Coordinator

    final class Coordinator: NSObject, PKCanvasViewDelegate {
        var parent: PencilCanvasRepresentable
        var toolPicker: PKToolPicker?
        var isDrawingActive = false

        init(_ parent: PencilCanvasRepresentable) {
            self.parent = parent
        }

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
