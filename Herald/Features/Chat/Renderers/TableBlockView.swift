import SwiftUI

/// Renders a markdown table as a Grid. First row is treated as the header.
///
/// Cells go through the same inline markdown parser as prose, so a link, bold
/// run or code span inside a table reads as one instead of showing its syntax.
/// Table syntax itself is block markdown and never reaches here.
struct TableBlockView: View {
    let rows: [[String]]

    private var headerRow: [String]? { rows.first }
    private var bodyRows: [[String]] { rows.count > 1 ? Array(rows.dropFirst()) : [] }
    private var columnCount: Int { rows.first?.count ?? 0 }

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                // Header
                if let header = headerRow {
                    GridRow {
                        ForEach(header.indices, id: \.self) { i in
                            cellText(header[i], font: .system(.caption, weight: .semibold))
                                .padding(.horizontal, Design.Spacing.sm)
                                .padding(.vertical, Design.Spacing.xs)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(Design.Colors.surface)
                                .border(Design.Colors.border, width: 0.5)
                        }
                    }
                }
                // Body rows
                ForEach(bodyRows.indices, id: \.self) { rowIdx in
                    GridRow {
                        let row = bodyRows[rowIdx]
                        ForEach(0..<columnCount, id: \.self) { colIdx in
                            cellText(
                                colIdx < row.count ? row[colIdx] : "",
                                font: .system(.caption)
                            )
                            .padding(.horizontal, Design.Spacing.sm)
                            .padding(.vertical, Design.Spacing.xs)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(rowIdx % 2 == 0
                                ? Design.Colors.surface.opacity(0.3)
                                : Color.clear)
                            .border(Design.Colors.border, width: 0.5)
                        }
                    }
                }
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
    }

    /// Inline markdown for one cell. Same parser options as chat prose, so
    /// `[4811006](url)` becomes a tappable id rather than a wall of URL.
    private func cellText(_ raw: String, font: Font) -> Text {
        if let attributed = try? AttributedString(
            markdown: raw,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        ) {
            return Text(attributed)
                .font(font)
                .foregroundStyle(Design.Colors.foreground)
        }
        return Text(raw)
            .font(font)
            .foregroundStyle(Design.Colors.foreground)
    }
}
