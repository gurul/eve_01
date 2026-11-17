// The two layouts every reply is made of: blocks one under the other, and a list item's marker beside its text.
// Each measures a child once per pass at the width it will have. SwiftUI's own stacks probe a child several times,
// and with lists inside lists that cost multiplies at every level.
import SwiftUI

/// Wraps one block and remembers its size for each width it is offered. A stack asks every child its size on
/// every pass; a finished block then answers from memory instead of measuring its lists and tables again. SwiftUI
/// drops the memory when the block inside changes.
struct MemoLayout: Layout {
    struct Cache {
        var sizes: [(offer: ProposedViewSize, size: CGSize)] = []
        var baselines: [(offer: ProposedViewSize, baseline: CGFloat)] = []
    }

    func makeCache(subviews: Subviews) -> Cache { Cache() }

    func updateCache(_ cache: inout Cache, subviews: Subviews) { cache = Cache() }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) -> CGSize {
        if let known = cache.sizes.first(where: { $0.offer == proposal }) { return known.size }
        guard let block = subviews.first else { return .zero }
        let size = block.sizeThatFits(proposal)
        if cache.sizes.count >= 6 { cache.sizes.removeFirst() }
        cache.sizes.append((proposal, size))
        return size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) {
        subviews.first?.place(at: bounds.origin, anchor: .topLeading,
                              proposal: ProposedViewSize(width: bounds.width, height: bounds.height))
    }

    func explicitAlignment(of guide: VerticalAlignment, in bounds: CGRect, proposal: ProposedViewSize,
                           subviews: Subviews, cache: inout Cache) -> CGFloat? {
        guard guide == .firstTextBaseline, let block = subviews.first else { return nil }
        let offer = ProposedViewSize(width: bounds.width, height: nil)
        if let known = cache.baselines.first(where: { $0.offer == offer }) { return bounds.minY + known.baseline }
        let baseline = block.dimensions(in: offer)[.firstTextBaseline]
        if cache.baselines.count >= 6 { cache.baselines.removeFirst() }
        cache.baselines.append((offer, baseline))
        return bounds.minY + baseline
    }
}

/// A list item: its marker (the first child) at its own width, its text (the second) in the rest, both on the
/// text's first line.
struct HangingLayout: Layout {
    var gap: CGFloat

    private struct Frames {
        var marker: CGSize
        var content: CGSize
        var contentOffer: ProposedViewSize
        var markerY: CGFloat
        var contentY: CGFloat
    }

    private func frames(_ subviews: Subviews, width: CGFloat?) -> Frames? {
        guard subviews.count == 2 else { return nil }
        let marker = subviews[0].dimensions(in: .unspecified)
        let offer = ProposedViewSize(width: width.map { max($0 - marker.width - gap, 0) }, height: nil)
        let content = subviews[1].dimensions(in: offer)
        let drop = content[.firstTextBaseline] - marker[.firstTextBaseline]
        return Frames(marker: CGSize(width: marker.width, height: marker.height),
                      content: CGSize(width: content.width, height: content.height), contentOffer: offer,
                      markerY: max(drop, 0), contentY: max(-drop, 0))
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let frames = frames(subviews, width: proposal.width) else { return .zero }
        return CGSize(width: frames.marker.width + gap + frames.content.width,
                      height: max(frames.markerY + frames.marker.height, frames.contentY + frames.content.height))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard let frames = frames(subviews, width: bounds.width) else { return }
        subviews[0].place(at: CGPoint(x: bounds.minX, y: bounds.minY + frames.markerY), anchor: .topLeading,
                          proposal: .unspecified)
        subviews[1].place(at: CGPoint(x: bounds.minX + frames.marker.width + gap, y: bounds.minY + frames.contentY),
                          anchor: .topLeading, proposal: frames.contentOffer)
    }

    func explicitAlignment(of guide: VerticalAlignment, in bounds: CGRect, proposal: ProposedViewSize,
                           subviews: Subviews, cache: inout ()) -> CGFloat? {
        guard guide == .firstTextBaseline, let frames = frames(subviews, width: bounds.width) else { return nil }
        return bounds.minY + frames.contentY
            + subviews[1].dimensions(in: frames.contentOffer)[.firstTextBaseline]
    }
}
