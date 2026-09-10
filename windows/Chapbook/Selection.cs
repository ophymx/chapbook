namespace Chapbook;

/// <summary>A range of the current unit's text, in locator offsets.</summary>
/// <remarks>
/// The space <see cref="Session.RangeRects"/> and
/// <see cref="Session.SelectRange(LocatorRange)"/> speak, and the one a
/// <see cref="SearchHit"/> carries. Not character offsets into any string:
/// <see cref="PageText"/> is where those are, and its
/// <see cref="PageText.LocatorRange"/> is the join.
/// </remarks>
public readonly record struct LocatorRange(uint Start, uint End);

// The live selection: what a press-drag builds, what a long press picks
// out, and what a search hit or a handle adjustment places directly.
//
// The selection is view state, not a mark — turning it into something the
// book remembers is `AddHighlight` or `AddNote`. Geometry for the grab
// handles comes from `RangeRects` over `SelectedRange`.
public sealed partial class Session
{
    /// <summary>
    /// Anchor a selection at a point, in logical panel coordinates.
    /// </summary>
    /// <returns>
    /// <c>false</c> when there was no text to anchor on — a press on bare
    /// page — which is the host's cue to treat the gesture as something
    /// else.
    /// </returns>
    /// <remarks>
    /// The anchor is empty until a drag extends it, so a press that never
    /// moves should be cleared rather than left to outlive the page.
    /// </remarks>
    public bool BeginSelection(float x, float y) =>
        Moved(
            Interop.cb_session_selection_begin(Live(), x, y, out byte started), started,
            nameof(BeginSelection));

    /// <summary>
    /// Extend the selection to a point: the move half of a press-drag, and
    /// equally the move half of dragging a grab handle.
    /// </summary>
    public void DragSelection(float x, float y) =>
        ChapbookException.Check(
            Interop.cb_session_selection_drag(Live(), x, y), nameof(DragSelection));

    /// <summary>
    /// Select the word under a point — what a long press means on glass,
    /// and a double click on a desk. <c>false</c> when no word was there.
    /// </summary>
    public bool SelectWordAt(float x, float y) =>
        Moved(
            Interop.cb_session_select_word_at(Live(), x, y, out byte selected), selected,
            nameof(SelectWordAt));

    /// <summary>
    /// Select an exact locator range — how a search hit or an adjusted
    /// handle becomes the selection. Offsets beyond the unit's text clamp
    /// rather than fail.
    /// </summary>
    public void SelectRange(uint start, uint end) =>
        ChapbookException.Check(
            Interop.cb_session_select_range(Live(), start, end), nameof(SelectRange));

    /// <inheritdoc cref="SelectRange(uint, uint)"/>
    public void SelectRange(LocatorRange range) => SelectRange(range.Start, range.End);

    /// <summary>Drop the selection. A no-op when there is none.</summary>
    public void ClearSelection() =>
        ChapbookException.Check(Interop.cb_session_selection_clear(Live()), nameof(ClearSelection));

    /// <summary>
    /// The selection as a locator range, or <c>null</c> when there is none
    /// — including the empty anchor a press leaves before any drag, which
    /// is deliberately not a selection yet.
    /// </summary>
    public LocatorRange? SelectedRange
    {
        get
        {
            Status status = Interop.cb_session_selected_range(
                Live(), out uint start, out uint end);
            if (status == Status.Unavailable)
            {
                return null;
            }
            ChapbookException.Check(status, nameof(SelectedRange));
            return new LocatorRange(start, end);
        }
    }

    /// <summary>
    /// The selected text, whitespace collapsed the way a clipboard wants
    /// it, or <c>null</c> when nothing is selected.
    /// </summary>
    public string? SelectedText =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_session_selected_text(Live(), b, c, out n), nameof(SelectedText));
}
