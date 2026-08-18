import PencilKit
import SwiftUI

// MARK: - Export Layer

/// Exportable layer types for a note.
struct NoteExportLayer: Identifiable, Hashable {
    let id: String
    let name: String
    let systemImage: String

    static let inkPDF = NoteExportLayer(id: "ink_pdf", name: "Ink PDF", systemImage: "doc.richtext")
    static let summaryPNG = NoteExportLayer(id: "summary_png", name: "Summary PNG", systemImage: "photo")
    static let recognizedText = NoteExportLayer(id: "recognized_text", name: "Recognized Text", systemImage: "text.quote")
    static let enrichedMarkdown = NoteExportLayer(id: "enriched_markdown", name: "Enriched Markdown", systemImage: "doc.text")
    static let citations = NoteExportLayer(id: "citations", name: "Citations", systemImage: "text.quote")

    static func availableLayers(
        hasDrawing: Bool,
        hasRecognition: Bool,
        hasEnrichment: Bool
    ) -> [NoteExportLayer] {
        var layers: [NoteExportLayer] = []
        if hasDrawing { layers.append(.inkPDF) }
        if hasEnrichment { layers.append(.summaryPNG) }
        if hasRecognition { layers.append(.recognizedText) }
        if hasEnrichment { layers.append(.enrichedMarkdown) }
        if hasEnrichment { layers.append(.citations) }
        return layers
    }
}

// MARK: - Export Sheet

struct NoteExportSheet: View {
    let result: EnrichmentResult?
    let drawing: PKDrawing?
    let recognizedText: String?

    @State private var selectedLayers: Set<String> = []
    @Environment(\.dismiss) private var dismiss

    init(
        result: EnrichmentResult? = nil,
        drawing: PKDrawing? = nil,
        recognizedText: String? = nil
    ) {
        self.result = result
        self.drawing = drawing
        self.recognizedText = recognizedText

        // Pre-select all available layers
        let available = NoteExportLayer.availableLayers(
            hasDrawing: drawing != nil,
            hasRecognition: recognizedText != nil,
            hasEnrichment: result != nil
        )
        _selectedLayers = State(initialValue: Set(available.map(\.id)))
    }

    private var availableLayers: [NoteExportLayer] {
        NoteExportLayer.availableLayers(
            hasDrawing: drawing != nil,
            hasRecognition: recognizedText != nil,
            hasEnrichment: result != nil
        )
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(availableLayers) { layer in
                        Toggle(isOn: Binding(
                            get: { selectedLayers.contains(layer.id) },
                            set: { isSelected in
                                if isSelected {
                                    selectedLayers.insert(layer.id)
                                } else {
                                    selectedLayers.remove(layer.id)
                                }
                            }
                        )) {
                            Label(layer.name, systemImage: layer.systemImage)
                        }
                    }
                } header: {
                    Text("Select layers to export")
                } footer: {
                    if result != nil {
                        Text("Enriched content is a static copy. Edits in the destination will not sync back to Herald.")
                    }
                }
            }
            .navigationTitle("Export Note")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Export") {
                        exportSelectedLayers()
                    }
                    .disabled(selectedLayers.isEmpty)
                }
            }
        }
    }

    private func exportSelectedLayers() {
        var items: [Any] = []

        if selectedLayers.contains(NoteExportLayer.inkPDF.id), let drawing {
            if let pdfData = exportDrawingAsPDF(drawing) {
                items.append(pdfData)
            }
        }

        if selectedLayers.contains(NoteExportLayer.summaryPNG.id), let result {
            if let pngData = exportSummaryAsPNG(result) {
                items.append(pngData)
            }
        }

        if selectedLayers.contains(NoteExportLayer.recognizedText.id), let recognizedText {
            items.append(recognizedText)
        }

        if selectedLayers.contains(NoteExportLayer.enrichedMarkdown.id), let result {
            items.append(result.markdown)
        }

        if selectedLayers.contains(NoteExportLayer.citations.id), let result, !result.citations.isEmpty {
            let citationsText = formatCitations(result.citations)
            items.append(citationsText)
        }

        guard !items.isEmpty else { return }

        let activityVC = UIActivityViewController(
            activityItems: items,
            applicationActivities: nil
        )

        // Present the share sheet
        if let windowScene = UIApplication.shared.connectedScenes.first as? UIWindowScene,
           let rootVC = windowScene.windows.first?.rootViewController {
            rootVC.present(activityVC, animated: true)
        }

        dismiss()
    }

    /// Render the enrichment summary board as a PNG so it can be shared to
    /// Photos, Files, Messages, or any app via the native share sheet.
    private func exportSummaryAsPNG(_ result: EnrichmentResult) -> Data? {
        let renderer = ImageRenderer(
            content: SummaryBoardExportView(result: result)
                .frame(width: 390)
        )
        renderer.scale = 2.0
        renderer.proposedSize = .init(width: 390, height: nil)
        guard let uiImage = renderer.uiImage else { return nil }
        return uiImage.pngData()
    }

    private func exportDrawingAsPDF(_ drawing: PKDrawing) -> Data? {
        let image = drawing.image(from: drawing.bounds, scale: 2.0)
        let data = NSMutableData()
        guard let consumer = CGDataConsumer(data: data as CFMutableData),
              let context = CGContext(consumer: consumer, mediaBox: nil, nil),
              let cgImage = image.cgImage else {
            return nil
        }
        var mediaBox = CGRect(origin: .zero, size: image.size)
        context.beginPDFPage(nil)
        context.draw(cgImage, in: mediaBox)
        context.endPDFPage()
        context.closePDF()
        return data as Data
    }

    private func formatCitations(_ citations: [EnrichedCitation]) -> String {
        var text = "Citations\n\n"
        for (index, citation) in citations.enumerated() {
            text += "\(index + 1). \(citation.title)"
            if let url = citation.url {
                text += " — \(url)"
            }
            text += " (accessed \(citation.accessedAt.formatted(date: .abbreviated, time: .omitted)))\n"
        }
        return text
    }
}

// MARK: - Share to Notes

/// Static export to Apple Notes with a disclaimer.
struct ShareToNotesSheet: View {
    let result: EnrichmentResult
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            VStack(spacing: Design.Spacing.lg) {
                // Preview
                VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                    Text(result.title)
                        .font(Design.Typography.screenTitle2)

                    Text(result.markdown)
                        .font(Design.Typography.body)
                        .lineLimit(10)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .padding()
                .background(Design.Colors.surface)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))

                // Disclaimer
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "info.circle")
                        .foregroundStyle(Design.Colors.secondaryForeground)

                    Text("This is a static export. Edits in Apple Notes will not sync back to Herald.")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .padding()
                .background(Design.Colors.surface2)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))

                Spacer()

                Button {
                    shareToNotes()
                } label: {
                    Label("Share to Apple Notes", systemImage: "square.and.arrow.up")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Design.Brand.accent)
                .padding(.horizontal)
            }
            .padding()
            .navigationTitle("Share to Notes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }

    private func shareToNotes() {
        let text = "\(result.title)\n\n\(result.markdown)\n\n— Shared from Kallisti (static copy)"
        let activityVC = UIActivityViewController(
            activityItems: [text],
            applicationActivities: nil
        )

        if let windowScene = UIApplication.shared.connectedScenes.first as? UIWindowScene,
           let rootVC = windowScene.windows.first?.rootViewController {
            rootVC.present(activityVC, animated: true)
        }

        dismiss()
    }
}

/// Self-contained summary board card rendered to PNG for the share sheet.
/// Mirrors the enrichment tab's look: title, timestamp, markdown body.
struct SummaryBoardExportView: View {
    let result: EnrichmentResult

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(result.title.isEmpty ? "Note Summary" : result.title)
                .font(.system(size: 20, weight: .bold))
                .foregroundStyle(.primary)

            Text(result.createdAt.formatted(date: .abbreviated, time: .shortened))
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(.secondary)

            Divider()

            Text(result.markdown)
                .font(.system(size: 14))
                .foregroundStyle(.primary)
                .textSelection(.enabled)

            Spacer(minLength: 0)
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(UIColor.systemBackground))
    }
}
