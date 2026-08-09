import SwiftUI

/// Inline diff view that renders file changes made by Herald during a coding task.
///
/// Shows a compact summary header ("2 files changed, +15 -3") that expands
/// to show individual file diffs with syntax-colored additions/deletions.
struct InlineDiffView: View {
    let diff: CodeDiff

    @State private var isExpanded = false
    @State private var expandedFiles: Set<String> = []

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            summaryHeader
            if isExpanded {
                fileList
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .animation(Design.Motion.quickResponse, value: isExpanded)
        .animation(Design.Motion.quickResponse, value: expandedFiles)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Code changes: \(diff.summary)")
    }

    // MARK: - Summary Header

    private var summaryHeader: some View {
        Button {
            withAnimation(Design.Motion.quickResponse) {
                isExpanded.toggle()
            }
        } label: {
            HStack(spacing: Design.Spacing.xs) {
                Image(systemName: "doc.text.magnifyingglass")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Design.Colors.foreground)

                Text(diff.summary)
                    .brandEyebrow()
                    .lineLimit(1)

                Spacer()

                changeStats

                Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
            }
            .padding(.horizontal, Design.Spacing.sm)
            .padding(.vertical, Design.Spacing.xxs + 2)
            .background(Design.Colors.surface)
            .overlay(
                RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                    .stroke(Design.Colors.border, lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
        }
        .buttonStyle(.plain)
    }

    private var changeStats: some View {
        HStack(spacing: Design.Spacing.xxs) {
            if diff.totalAdditions > 0 {
                Text("+\(diff.totalAdditions)")
                    .font(Design.Typography.caption2.weight(.medium))
                    .foregroundStyle(Design.Colors.success)
            }
            if diff.totalDeletions > 0 {
                Text("-\(diff.totalDeletions)")
                    .font(Design.Typography.caption2.weight(.medium))
                    .foregroundStyle(Design.Colors.danger)
            }
        }
    }

    // MARK: - File List

    private var fileList: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
            ForEach(diff.files) { file in
                fileRow(file)
            }
        }
        .padding(.vertical, Design.Spacing.xxs)
        .padding(.horizontal, Design.Spacing.xxs)
        .background(Design.Colors.surface)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
    }

    private func fileRow(_ file: FileDiff) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
            Button {
                withAnimation(Design.Motion.quickResponse) {
                    if expandedFiles.contains(file.path) {
                        expandedFiles.remove(file.path)
                    } else {
                        expandedFiles.insert(file.path)
                    }
                }
            } label: {
                HStack(spacing: Design.Spacing.xs) {
                    Image(systemName: file.statusIcon)
                        .font(.system(size: 9))
                        .foregroundStyle(colorForStatus(file.status))

                    VStack(alignment: .leading, spacing: 0) {
                        Text(file.fileName)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.foreground)
                            .lineLimit(1)

                        if !file.directoryPath.isEmpty {
                            Text(file.directoryPath)
                                .font(Design.Typography.caption2)
                                .foregroundStyle(Design.Colors.tertiaryForeground)
                                .lineLimit(1)
                        }
                    }

                    Spacer()

                    HStack(spacing: Design.Spacing.xxs) {
                        if file.additions > 0 {
                            Text("+\(file.additions)")
                                .font(Design.Typography.caption2)
                                .foregroundStyle(Design.Colors.success)
                        }
                        if file.deletions > 0 {
                            Text("-\(file.deletions)")
                                .font(Design.Typography.caption2)
                                .foregroundStyle(Design.Colors.danger)
                        }
                    }

                    Image(systemName: expandedFiles.contains(file.path) ? "chevron.up" : "chevron.down")
                        .font(.system(size: 7, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
                }
                .padding(.horizontal, Design.Spacing.xs)
                .padding(.vertical, Design.Spacing.xxs)
            }
            .buttonStyle(.plain)

            if expandedFiles.contains(file.path) {
                patchView(file.patch)
                    .transition(.opacity)
            }
        }
    }

    // MARK: - Patch View

    private func patchView(_ patch: String) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(parsePatchLines(patch).enumerated()), id: \.offset) { _, line in
                    patchLine(line)
                }
            }
            .padding(.horizontal, Design.Spacing.xs)
            .padding(.vertical, Design.Spacing.xxs)
        }
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.xs)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xs))
        .padding(.horizontal, Design.Spacing.xs)
        .padding(.bottom, Design.Spacing.xxs)
    }

    private func patchLine(_ line: DiffLine) -> some View {
        HStack(spacing: 0) {
            Text(line.prefix)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(line.prefixColor)
                .frame(width: 12, alignment: .center)

            Text(line.content)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(line.contentColor)
                .lineLimit(1)
        }
        .padding(.vertical, 0.5)
        .background(line.backgroundColor)
    }

    // MARK: - Helpers

    private func colorForStatus(_ fileStatus: String) -> Color {
        switch fileStatus {
        case "added": return Design.Colors.success
        case "deleted": return Design.Colors.danger
        case "renamed": return Design.Brand.primary
        default: return Design.Colors.warning
        }
    }

    private struct DiffLine {
        let prefix: String
        let content: String
        let prefixColor: Color
        let contentColor: Color
        let backgroundColor: Color
    }

    private func parsePatchLines(_ patch: String) -> [DiffLine] {
        let lines = patch.components(separatedBy: "\n")
        var result: [DiffLine] = []

        // Skip diff header lines (---, +++, diff --git, index)
        for line in lines {
            if line.hasPrefix("diff --git") || line.hasPrefix("index ")
                || line.hasPrefix("---") || line.hasPrefix("+++")
                || line.hasPrefix("new file") || line.hasPrefix("deleted file") {
                continue
            }

            if line.hasPrefix("@@") {
                // Hunk header
                result.append(DiffLine(
                    prefix: "",
                    content: line,
                    prefixColor: Design.Colors.secondaryForeground,
                    contentColor: Design.Colors.secondaryForeground,
                    backgroundColor: Design.Brand.primary.opacity(0.08)
                ))
            } else if line.hasPrefix("+") {
                result.append(DiffLine(
                    prefix: "+",
                    content: String(line.dropFirst()),
                    prefixColor: Design.Colors.success,
                    contentColor: Design.Colors.foreground,
                    backgroundColor: Design.Colors.success.opacity(0.10)
                ))
            } else if line.hasPrefix("-") {
                result.append(DiffLine(
                    prefix: "-",
                    content: String(line.dropFirst()),
                    prefixColor: Design.Colors.danger,
                    contentColor: Design.Colors.foreground,
                    backgroundColor: Design.Colors.danger.opacity(0.10)
                ))
            } else if !line.isEmpty {
                result.append(DiffLine(
                    prefix: " ",
                    content: line.hasPrefix(" ") ? String(line.dropFirst()) : line,
                    prefixColor: .clear,
                    contentColor: Design.Colors.secondaryForeground,
                    backgroundColor: .clear
                ))
            }
        }

        return result
    }
}
