import CChapbook
import CoreGraphics
import Foundation

// Pinch and pan, for image books.
//
// Zoom is view state: nothing persists it, and it survives a page turn on
// purpose — an app that wants turns to reset sets 1.0 on turn.
//
// The asymmetry worth knowing before drawing anything over a zoomed page:
// input crossing this boundary is mapped through the zoom automatically,
// while output geometry — `rects(for:)`, the text surface — stays in
// fit-page space. An app maps its own overlays forward itself, with
// `view = fit * zoom + pan`.
extension Session {
    /// Zoom around a focal point in logical panel coordinates — the
    /// pinch. Clamped to `1.0...8.0`, where 1.0 is fit.
    ///
    /// Returns whether the view moved. **Always `false` on reflowable
    /// text**, where the same gesture means "make the text bigger" — a
    /// settings change the app maps to [`Action.fontUp`] /
    /// [`Action.fontDown`] itself.
    @discardableResult
    public func setPageZoom(_ zoom: CGFloat, focus: CGPoint) throws -> Bool {
        var changed = false
        try check(
            cb_session_set_page_zoom(
                raw, Float(zoom), Float(focus.x), Float(focus.y), &changed))
        return changed
    }

    /// Pan the zoomed page by a pointer delta in logical panel
    /// coordinates, clamped at the page's edges.
    ///
    /// `false` at fit — which is how an app knows the same drag should
    /// fall through to whatever an unzoomed drag means: a selection, a
    /// swipe turn.
    @discardableResult
    public func panPage(by delta: CGSize) throws -> Bool {
        var changed = false
        try check(cb_session_pan_page(raw, Float(delta.width), Float(delta.height), &changed))
        return changed
    }

    /// The current zoom, 1.0 at fit.
    public func pageZoom() throws -> CGFloat {
        var zoom: Float = 0
        try check(cb_session_page_zoom(raw, &zoom))
        return CGFloat(zoom)
    }

    /// The current pan in page units — with the zoom, the forward map for
    /// an app's own overlays.
    public func pagePan() throws -> CGPoint {
        var x: Float = 0
        var y: Float = 0
        try check(cb_session_page_pan(raw, &x, &y))
        return CGPoint(x: CGFloat(x), y: CGFloat(y))
    }
}
