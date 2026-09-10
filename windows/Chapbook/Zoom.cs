namespace Chapbook;

// Pinch and pan, for image books.
//
// Zoom is view state: nothing persists it, and it survives a page turn on
// purpose — a host that wants turns to reset sets 1.0 on turn.
//
// The asymmetry worth knowing before drawing anything over a zoomed page:
// input crossing this boundary is mapped through the zoom automatically,
// while output geometry — `RangeRects`, the text surface — stays in
// fit-page space. A host maps its own overlays forward itself, with
// `view = fit * zoom + pan`.
public sealed partial class Session
{
    /// <summary>
    /// Zoom around a focal point in logical panel coordinates — the pinch,
    /// or a wheel with a modifier held. Clamped to <c>[1.0, 8.0]</c>,
    /// where 1.0 is fit.
    /// </summary>
    /// <returns>
    /// Whether the view moved. <b>Always <c>false</c> on reflowable
    /// text</b>, where the same gesture means "make the text bigger" — a
    /// settings change the host maps to <see cref="ReaderAction.FontUp"/>
    /// and <see cref="ReaderAction.FontDown"/> itself.
    /// </returns>
    public bool SetPageZoom(float zoom, float focusX, float focusY) =>
        Moved(
            Interop.cb_session_set_page_zoom(Live(), zoom, focusX, focusY, out byte changed),
            changed, nameof(SetPageZoom));

    /// <summary>
    /// Pan the zoomed page by a pointer delta in logical panel
    /// coordinates, clamped at the page's edges.
    /// </summary>
    /// <returns>
    /// <c>false</c> at fit — which is how a host knows the same drag
    /// should fall through to whatever an unzoomed drag means: a
    /// selection, a swipe turn.
    /// </returns>
    public bool PanPage(float dx, float dy) =>
        Moved(
            Interop.cb_session_pan_page(Live(), dx, dy, out byte changed), changed,
            nameof(PanPage));

    /// <summary>The current zoom, 1.0 at fit.</summary>
    public float PageZoom
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_page_zoom(Live(), out float zoom), nameof(PageZoom));
            return zoom;
        }
    }

    /// <summary>
    /// The current pan in page units — with the zoom, the forward map for
    /// a host's own overlays.
    /// </summary>
    public (float X, float Y) PagePan
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_page_pan(Live(), out float x, out float y), nameof(PagePan));
            return (x, y);
        }
    }
}
