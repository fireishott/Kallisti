import SwiftUI

/// Paper background style for the note canvas.
/// Supports ruled lines, grid, or blank — configurable per note.
struct NotePaperBackground: View {
    let style: NotePageStyle
    var showRuledLines: Bool = true

    var body: some View {
        Canvas { context, size in
            // Subtle paper color
            context.fill(
                Path(CGRect(origin: .zero, size: size)),
                with: .color(Color(.systemBackground))
            )

            guard showRuledLines else { return }

            let lineSpacing: CGFloat = 24  // ~1/3 inch at 72 PPI
            let lineColor = Color.secondary.opacity(0.08)
            let marginColor = Color.red.opacity(0.06)
            let leftMargin: CGFloat = 72   // 1 inch

            // Horizontal ruled lines
            var y: CGFloat = lineSpacing
            while y < size.height {
                var linePath = Path()
                linePath.move(to: CGPoint(x: 0, y: y))
                linePath.addLine(to: CGPoint(x: size.width, y: y))
                context.stroke(linePath, with: .color(lineColor), lineWidth: 0.5)
                y += lineSpacing
            }

            // Left margin line
            var marginPath = Path()
            marginPath.move(to: CGPoint(x: leftMargin, y: 0))
            marginPath.addLine(to: CGPoint(x: leftMargin, y: size.height))
            context.stroke(marginPath, with: .color(marginColor), lineWidth: 1.0)
        }
    }
}
