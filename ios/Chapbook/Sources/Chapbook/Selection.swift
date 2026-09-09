import CChapbook
import CoreGraphics
import Foundation

// The live selection: what a press-drag builds, what a long press picks
// out, and what a search hit or a handle adjustment places directly.
//
// The selection is view state, not a mark — turning it into something the
// book remembers is `addHighlight()` or `addNote(_:)`. Geometry for the
// grab handles comes from `rects(for:)` over `selectedRange()`.
extension Session {
    /// Anchor a selection at a point, in logical panel coordinates.
    ///
    /// `false` means there was no text to anchor on — a press on bare
    /// page — which is the app's cue to treat the gesture as something
    /// else. The anchor is empty until a drag extends it, so a press that
    /// never moves should be cleared rather than left to outlive the page.
    @discardableResult
    public func beginSelection(at point: CGPoint) throws -> Bool {
        var started = false
        try check(cb_session_selection_begin(raw, Float(point.x), Float(point.y), &started))
        return started
    }

    /// Extend the selection to a point: the move half of a press-drag,
    /// and equally the move half of dragging a grab handle.
    public func dragSelection(to point: CGPoint) throws {
        try check(cb_session_selection_drag(raw, Float(point.x), Float(point.y)))
    }

    /// Select the word under a point — what a long press means on glass.
    /// `false` when no word was there.
    @discardableResult
    public func selectWord(at point: CGPoint) throws -> Bool {
        var selected = false
        try check(cb_session_select_word_at(raw, Float(point.x), Float(point.y), &selected))
        return selected
    }

    /// Select an exact locator range — how a search hit or an adjusted
    /// handle becomes the selection. Offsets beyond the unit's text clamp
    /// rather than fail.
    public func select(_ locators: Range<UInt32>) throws {
        try check(cb_session_select_range(raw, locators.lowerBound, locators.upperBound))
    }

    /// Drop the selection. A no-op when there is none.
    public func clearSelection() throws {
        try check(cb_session_selection_clear(raw))
    }

    /// The selection as a locator range, or `nil` when there is none —
    /// including the empty anchor a press leaves before any drag, which
    /// is deliberately not a selection yet.
    public func selectedRange() throws -> Range<UInt32>? {
        var start: UInt32 = 0
        var end: UInt32 = 0
        let status = cb_session_selected_range(raw, &start, &end)
        if status == C.unavailable { return nil }
        try check(status)
        return start..<end
    }

    /// The selected text, whitespace collapsed the way a clipboard wants
    /// it, or `nil` when nothing is selected.
    public func selectedText() throws -> String? {
        try readOptionalString { cb_session_selected_text(self.raw, $0, $1, $2) }
    }
}
