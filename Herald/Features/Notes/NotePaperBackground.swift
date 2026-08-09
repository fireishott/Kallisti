import SwiftUI

/// Paper background that draws ruled lines, grid, or nothing based on the style.
/// Theme-aware: dark mode uses light lines, light mode uses dark lines.
struct NotePaperBackground: View {
    let style: NotePageStyle
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        Canvas { context, size in
            context.fill(
                Path(CGRect(origin: .zero, size: size)),
                with: .color(Color(.systemBackground))
            )

            let spacing = style.lineSpacing
            guard spacing > 0 else { return }

            let lineColor = resolvedLineColor
            let marginColor = Color.red.opacity(colorScheme == .dark ? 0.12 : 0.06)
            let lineWidth: CGFloat = colorScheme == .dark ? 0.75 : 0.5

            // Horizontal ruled lines
            if style.showsRuledLines || style.showsGrid {
                var y: CGFloat = spacing
                while y < size.height {
                    var path = Path()
                    path.move(to: CGPoint(x: 0, y: y))
                    path.addLine(to: CGPoint(x: size.width, y: y))
                    context.stroke(path, with: .color(lineColor), lineWidth: lineWidth)
                    y += spacing
                }
            }

            // Vertical grid lines
            if style.showsGrid {
                var x: CGFloat = spacing
                while x < size.width {
                    var path = Path()
                    path.move(to: CGPoint(x: x, y: 0))
                    path.addLine(to: CGPoint(x: x, y: size.height))
                    context.stroke(path, with: .color(lineColor), lineWidth: lineWidth)
                    x += spacing
                }
            }

            // Red margin line (only for ruled-line styles)
            if style.showsMarginLine {
                let leftMargin: CGFloat = 72 // 1 inch at 72 PPI
                var marginPath = Path()
                marginPath.move(to: CGPoint(x: leftMargin, y: 0))
                marginPath.addLine(to: CGPoint(x: leftMargin, y: size.height))
                context.stroke(marginPath, with: .color(marginColor), lineWidth: 1.0)
            }
        }
    }

    private var resolvedLineColor: Color {
        colorScheme == .dark
            ? Color.white.opacity(0.12)
            : Color(.systemGray3)
    }
}
