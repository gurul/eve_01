// Where a table's cells go. Every column is as wide as its widest cell when the table fits; when it does not,
// columns narrow toward a floor and their text wraps; when even the floors do not fit, the table is laid out at
// a readable width and its caller scrolls it sideways. Rows align on the first line of text.
import SwiftUI

/// Marks a child of the table: a cell (the default) or the hairline under a row.
struct TableLineAfterRow: LayoutValueKey {
    static let defaultValue: Int? = nil
}

struct TableLayout: Layout {
    enum Mode {
        /// Fit the offered width by wrapping cells. Its ideal width is the narrowest it can be.
        case fit
        /// Take the width the columns want, each up to the cell limit.
        case scroll
    }

    var columns: Int
    var alignments: [MarkdownTable.Alignment]
    var mode: Mode
    var columnGap: CGFloat
    var rowInset: CGFloat
    /// The narrowest a column is squeezed to before the table scrolls instead.
    var columnFloor: CGFloat
    /// The widest a cell is before its text wraps.
    var cellLimit: CGFloat

    struct Frames {
        var width: CGFloat = 0
        var size: CGSize = .zero
        var columnX: [CGFloat] = []
        var columnWidth: [CGFloat] = []
        var rowTop: [CGFloat] = []
        var rowHeight: [CGFloat] = []
        var baseline: [CGFloat] = []
    }

    struct Cache {
        /// Each column's widest cell on one line; nil until measured.
        var natural: [CGFloat]?
        var frames: [CGFloat: Frames] = [:]
    }

    func makeCache(subviews: Subviews) -> Cache { Cache() }

    func updateCache(_ cache: inout Cache, subviews: Subviews) { cache = Cache() }

    private func cells(_ subviews: Subviews) -> [LayoutSubview] {
        subviews.filter { $0[TableLineAfterRow.self] == nil }
    }

    private func natural(_ cells: [LayoutSubview], cache: inout Cache) -> [CGFloat] {
        if let known = cache.natural { return known }
        var widths = [CGFloat](repeating: 0, count: columns)
        for (i, cell) in cells.enumerated() {
            let column = i % columns
            widths[column] = max(widths[column], min(cell.sizeThatFits(.unspecified).width.rounded(.up), cellLimit))
        }
        cache.natural = widths
        return widths
    }

    private func frames(_ subviews: Subviews, offered: CGFloat?, cache: inout Cache) -> Frames {
        let cells = cells(subviews)
        guard columns > 0, !cells.isEmpty else { return Frames() }
        let natural = natural(cells, cache: &cache)
        let gaps = columnGap * CGFloat(columns - 1)
        let floors = natural.map { min($0, columnFloor) }
        var widths = natural
        var key: CGFloat = -1
        if mode == .fit {
            // With no width offered, say how narrow the table can be: its caller asks that to see if it fits.
            let room = (offered ?? floors.reduce(0, +) + gaps) - gaps
            key = room
            let wanted = natural.reduce(0, +)
            if wanted > room {
                let least = floors.reduce(0, +)
                let spare = max(room - least, 0)
                let stretch = wanted - least
                widths = zip(natural, floors).map { want, floor in
                    stretch > 0 ? floor + spare * (want - floor) / stretch : floor
                }
            }
        }
        if let known = cache.frames[key] { return known }

        var out = Frames(width: key)
        var x: CGFloat = 0
        for width in widths {
            out.columnX.append(x)
            out.columnWidth.append(width)
            x += width + columnGap
        }
        var y: CGFloat = 0
        let rows = (cells.count + columns - 1) / columns
        for row in 0..<rows {
            var ascent: CGFloat = 0
            var descent: CGFloat = 0
            for column in 0..<columns where row * columns + column < cells.count {
                let size = cells[row * columns + column].dimensions(in: ProposedViewSize(width: widths[column], height: nil))
                let first = size[.firstTextBaseline]
                ascent = max(ascent, first)
                descent = max(descent, size.height - first)
            }
            out.rowTop.append(y)
            out.baseline.append(ascent)
            out.rowHeight.append(rowInset + ascent + descent + rowInset)
            y += rowInset + ascent + descent + rowInset
        }
        out.size = CGSize(width: widths.reduce(0, +) + gaps, height: y)
        cache.frames[key] = out
        return out
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) -> CGSize {
        frames(subviews, offered: proposal.width.flatMap { $0.isFinite ? $0 : nil }, cache: &cache).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) {
        let frames = frames(subviews, offered: mode == .fit ? bounds.width : nil, cache: &cache)
        var cell = 0
        for subview in subviews {
            if let row = subview[TableLineAfterRow.self] {
                guard frames.rowTop.indices.contains(row) else { continue }
                let y = bounds.minY + frames.rowTop[row] + frames.rowHeight[row]
                subview.place(at: CGPoint(x: bounds.minX, y: y), anchor: .leading,
                              proposal: ProposedViewSize(width: frames.size.width, height: nil))
                continue
            }
            let row = cell / columns
            let column = cell % columns
            cell += 1
            guard frames.rowTop.indices.contains(row) else { continue }
            let width = frames.columnWidth[column]
            let offer = ProposedViewSize(width: width, height: nil)
            let size = subview.dimensions(in: offer)
            var x = bounds.minX + frames.columnX[column]
            switch alignments.indices.contains(column) ? alignments[column] : .leading {
            case .leading: break
            case .center: x += (width - size.width) / 2
            case .trailing: x += width - size.width
            }
            let y = bounds.minY + frames.rowTop[row] + rowInset + frames.baseline[row] - size[.firstTextBaseline]
            subview.place(at: CGPoint(x: x, y: y), anchor: .topLeading, proposal: offer)
        }
    }
}
