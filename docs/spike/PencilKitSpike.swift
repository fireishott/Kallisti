// PencilKit API Spike — Herald 1.8.0
// Throwaway file: not in any production target.
// Purpose: determine which PencilKit APIs are available on the installed SDK.

import PencilKit
import UIKit

// MARK: - Test 1: PKCanvasView basics

func testCanvasView() {
    let canvas = PKCanvasView()
    canvas.drawingPolicy = .anyInput  // or .pencilOnly
    let picker = PKToolPicker()
    picker.addObserver(canvas)
    picker.setVisible(true, forFirstResponder: canvas)
    _ = canvas  // suppress unused warning
}

// MARK: - Test 2: PKDrawing serialization

func testDrawingSerialization() {
    let drawing = PKDrawing()
    let data = drawing.dataRepresentation()
    let restored = try? PKDrawing(data: data)
    assert(drawing == restored)
}

// MARK: - Test 3: PKStrokeRecognizer (handwriting recognition)
// FINDING: PKStrokeRecognizer does NOT exist in the public iOS 26.5 SDK.
// RecognitionController exists as a private symbol in PencilKit.tbd but
// has no public header or Swift interface. This API is not available to apps.

// MARK: - Test 4: Pencil double-tap and squeeze APIs

func testPencilInteractions() {
    // UIPencilInteraction.preferredTapAction — available since iOS 12.1
    let tapAction = UIPencilInteraction.preferredTapAction
    print("Preferred tap action: \(tapAction)")

    // UIPencilInteraction.preferredSqueezeAction — Pencil Pro, iOS 17+
    let squeezeAction = UIPencilInteraction.preferredSqueezeAction
    print("Preferred squeeze action: \(squeezeAction)")
}

// MARK: - Test 5: Undo/Redo via PKCanvasView

func testUndoRedo() {
    let canvas = PKCanvasView()
    canvas.undoManager?.undo()
    canvas.undoManager?.redo()
    _ = canvas
}

// MARK: - Test 6: PKStroke properties

func testPKStroke() {
    let stroke = PKStroke(ink: PKInk(.pen, color: .black), path: PKStrokePath())
    _ = stroke.ink
    _ = stroke.path
    _ = stroke.transform
    _ = stroke.mask
}

// MARK: - Test 7: PKInk types

func testPKInkTypes() {
    let pen = PKInk(.pen, color: .black)
    let pencil = PKInk(.pencil, color: .gray)
    let marker = PKInk(.marker, color: .yellow)
    _ = pen; _ = pencil; _ = marker
}

// MARK: - Main (never actually runs — this is a compilation test)

print("PencilKit API Spike — compilation check only")
testCanvasView()
testDrawingSerialization()
// testStrokeRecognizer() — removed: PKStrokeRecognizer not in public SDK
testPencilInteractions()
testUndoRedo()
testPKStroke()
testPKInkTypes()
