# Notes Contract V1 — Herald 1.8.0

**Date:** 2026-07-21
**SDK:** iOS 26.5 (Xcode 26.5)
**Spike file:** `docs/spike/PencilKitSpike.swift`

---

## 1. API Availability Matrix

| API | Status | Notes |
|-----|--------|-------|
| `PKCanvasView` | **Available** | Drawing canvas with `.anyInput` / `.pencilOnly` policy |
| `PKDrawing` | **Available** | `.dataRepresentation()` / `init(data:)` for serialization |
| `PKToolPicker` | **Available** | Floating tool palette, `addObserver` / `setVisible` |
| `PKStroke` | **Available** | `.ink`, `.path`, `.transform`, `.mask` properties |
| `PKStrokePath` | **Available** | `RandomAccessCollection` of `PKStrokePoint` |
| `PKInk` | **Available** | `.pen`, `.pencil`, `.marker` ink types |
| `PKInkingTool` | **Available** | Struct, `PKTool` conformance |
| `PKEraserTool` | **Available** | Struct, `PKTool` conformance |
| `PKLassoTool` | **Available** | Struct, `PKTool` conformance |
| `PKToolPickerCustomItem` | **Available** | Custom tool picker items |
| `PKStrokeRecognizer` | **NOT available** | Not in public headers or Swift interface |
| `RecognitionController` | **NOT available** | Private symbol in PencilKit.tbd only |
| `UIPencilInteraction.preferredTapAction` | **Available** | Since iOS 12.1 |
| `UIPencilInteraction.preferredSqueezeAction` | **Available** | iOS 17.5+, Pencil Pro |
| `VNRecognizeTextRequest` (Vision) | **Available** | Fallback for text recognition |

---

## 2. Apple Notes Refinement Falsification Test

**Symbols searched in installed SDK:**

| Symbol | Result |
|--------|--------|
| `SmartScript` | Not found |
| `RefineHandwriting` | Not found |
| `AutoRefine` | Found as private symbol in `PencilKit.tbd` (`RecognitionController.AutoRefineMode`) — **NOT public API** |
| `NotesInterop` | Not found |
| `HandwritingCleanup` | Not found |

**Conclusion:** Apple Notes handwriting refinement API is **not publicly available**. Herald 1.8.0 will NOT embed Apple Notes refinement.

---

## 3. Physical-Device Spike Results

**Status:** NOT YET TESTED — requires physical iPad + Apple Pencil.

**What needs manual verification:**

- [ ] Save/reopen fidelity of `PKDrawing.dataRepresentation()` round-trip
- [ ] Recognition latency for a page of handwriting (if using Vision fallback)
- [ ] Memory usage with large drawings (1000+ strokes)
- [ ] Behavior with large drawings (frame drops, jank)
- [ ] Background cancellation of recognition tasks
- [ ] Pencil double-tap gesture behavior
- [ ] Pencil Pro squeeze gesture behavior (requires Pencil Pro hardware)
- [ ] `PKCanvasView.drawingPolicy = .pencilOnly` touch rejection quality

**Test matrix:**

| Device | Pencil | iOS | Tests |
|--------|--------|-----|-------|
| iPad Air (oldest supported) | Apple Pencil 1st gen | 18.0+ | All above |
| iPad Pro | Apple Pencil Pro | 18.0+ | Squeeze gesture |

---

## 4. Product Decisions

### 4.1 Recognition Strategy

- **Primary:** Vision framework `VNRecognizeTextRequest` for handwriting-to-text
- **Rationale:** `PKStrokeRecognizer` is not public API; Vision is the only supported path
- **Rendering:** Render `PKDrawing` to image → feed to `VNRecognizeTextRequest`
- **Latency:** Expect 1-3 seconds per page on modern iPads (needs device verification)

### 4.2 Languages

- Limited to `VNRecognizeTextRequest` supported languages
- Revision 2+: English, Chinese, Portuguese, French, Italian, German, Spanish
- Revision 3+: Auto-detection of script/language
- **Decision:** Ship with Revision 3+ where available, fall back to Revision 2

### 4.3 Refinement

- Apple Notes refinement is NOT available as public API
- **Decision:** No embedded refinement in 1.8.0
- **Alternative:** Export-to-Notes for users who want refinement
- **Future:** Monitor WWDC for public refinement API

### 4.4 Sync

- Local-first only in 1.8.0
- `PKDrawing.dataRepresentation()` is the serialization format
- No CloudKit sync for drawings in this release

### 4.5 iPhone

- Read-only recognized/enriched view on iPhone
- No drawing input on iPhone (no Pencil support)
- Display recognized text + original drawing as image

### 4.6 Commands

- Explicit "Enrich" user action (not automatic)
- Allowlisted commands only:
  - "Enrich" — run recognition on current drawing
  - "Export to Notes" — share recognized text to Apple Notes
- No implicit or background recognition

---

## 5. Exit Gate Checklist

- [x] Spike compiles against iOS 26.5 SDK
- [x] `PKStrokeRecognizer` availability determined (NOT public)
- [x] Apple Notes refinement falsification test complete (NOT available)
- [x] Vision fallback identified (`VNRecognizeTextRequest`)
- [ ] Physical device testing complete (blocked: needs iPad + Pencil)
- [x] Product decisions documented
- [x] API availability matrix complete

**Gate status:** PASS with deferred physical-device testing

---

## 6. Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Vision text recognition accuracy for handwriting | High | Test with diverse handwriting samples on device |
| Large drawing memory pressure | Medium | Implement drawing chunking if >100MB |
| No refinement = user disappointment | Medium | Clear messaging: "Export to Notes for refinement" |
| Pencil Pro squeeze gesture adoption | Low | Fallback to double-tap and toolbar button |
