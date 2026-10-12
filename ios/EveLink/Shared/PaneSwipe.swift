// Swiping between the home panes (owner, 2026-10-12: "allow me to swipe between panes"). A pane swipe is a clearly
// horizontal drag: far enough, and mostly sideways, so a vertical scroll that drifts never changes the pane.
import CoreGraphics

enum PaneSwipe {
    /// How far a drag must travel sideways, in points.
    static let minDistance: CGFloat = 70
    /// How much more sideways than up or down it must be.
    static let ratio: CGFloat = 2

    /// The pane index after a drag of (dx, dy) from `index` among `count` panes, or nil when it stays.
    /// Finger moving left (dx < 0) goes to the pane on the right, as pages do.
    static func target(index: Int, count: Int, dx: CGFloat, dy: CGFloat) -> Int? {
        guard abs(dx) >= minDistance, abs(dx) >= abs(dy) * ratio else { return nil }
        let next = index + (dx < 0 ? 1 : -1)
        return (0..<count).contains(next) ? next : nil
    }
}
