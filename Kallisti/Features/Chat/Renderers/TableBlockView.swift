import SwiftUI

/// Renders a markdown table as a Grid. First row is treated as the header.
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
                            Text(header[i])
                                .font(.system(.caption, weight: .semibold))
                                .foregroundStyle(Design.Colors.foreground)
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
                            Text(colIdx < row.count ? row[colIdx] : "")
                                .font(.system(.caption))
                                .foregroundStyle(Design.Colors.foreground)
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
}
