namespace Chapbook;

/// <summary>One mark on this book.</summary>
/// <param name="Id">
/// Stable for the mark's life, sync included — which is why every mutating
/// call takes this and not an index.
/// </param>
/// <param name="Spine">The spine unit the mark resolves against in this book.</param>
/// <param name="Progression">
/// Whole-book progression of its start, 0 to 1 — what orders a marks list
/// and places its gutter dots.
/// </param>
/// <param name="Text">
/// The quoted text of a highlight, or the body of a note. <c>null</c> for
/// a bookmark, which has neither.
/// </param>
/// <param name="Color">
/// The chosen colour as the hex string it was set with, or <c>null</c>
/// for a mark wearing the theme's.
/// </param>
public sealed record Annotation(
    long Id, AnnotationKind Kind, int Spine, double Progression, string? Text, string? Color);

// The marks a reader leaves: bookmarks, highlights, notes. These persist
// in the library and travel to a book's annotation container on the next
// sync, so a removal here is a removal everywhere — nothing about them is
// local scratch state.
//
// Two things have to be true before a mark can exist, and they fail
// differently. The build needs the library capability
// (`Engine.Capabilities.HasFlag(Capabilities.Library)`), or every call
// here throws saying a mark nothing would remember is not a mark; and the
// session needs a library directory, without which there is nowhere to
// put one. Neither is checked for you.
public sealed partial class Session
{
    /// <summary>This book's marks, ordered by progression.</summary>
    /// <remarks>
    /// Re-read from the library per call, so <b>re-enumerate after any
    /// add or remove</b>: indices are stable between mutations and not
    /// across them. Ids are stable throughout, which is what to hold on
    /// to.
    /// </remarks>
    public IReadOnlyList<Annotation> Annotations()
    {
        ChapbookException.Check(
            Interop.cb_session_annotation_count(Live(), out nuint count), nameof(Annotations));
        var marks = new List<Annotation>((int)count);
        for (nuint i = 0; i < count; i++)
        {
            nuint index = i;
            ChapbookException.Check(
                Interop.cb_session_annotation(Live(), index, out NativeAnnotation row),
                nameof(Annotations));
            string? text = row.HasText == 0 ? null :
                Strings.Read((byte[]? b, nuint c, out nuint n) =>
                    Interop.cb_session_annotation_text(Live(), index, b, c, out n),
                    nameof(Annotations));
            string? color = row.HasColor == 0 ? null :
                Strings.Read((byte[]? b, nuint c, out nuint n) =>
                    Interop.cb_session_annotation_color(Live(), index, b, c, out n),
                    nameof(Annotations));
            marks.Add(new Annotation(
                row.Id, row.Kind, (int)row.Spine, row.Progression, text, color));
        }
        return marks;
    }

    // ---- Making them ----

    /// <summary>Bookmark the current position — a point, nothing painted.</summary>
    /// <remarks>
    /// Throws when there is no position to keep yet: a session with no
    /// metrics has not laid anything out, so there is nowhere to point.
    /// </remarks>
    public long AddBookmark()
    {
        ChapbookException.Check(
            Interop.cb_session_add_bookmark(Live(), out long id), nameof(AddBookmark));
        return id;
    }

    /// <summary>
    /// Turn the live selection into a stored highlight, in the theme's
    /// colour until one is chosen.
    /// </summary>
    /// <remarks>
    /// <para>
    /// The selection is still the host's afterwards: clearing it is a
    /// separate move, so the paint order — highlight replaces selection —
    /// is explicit rather than implied.
    /// </para>
    /// <para>
    /// Throws when there is nothing to make a highlight out of: nothing
    /// selected, no library to keep it in, or an image book, which has no
    /// text to cover. Reported rather than returned as a quiet <c>null</c>,
    /// because a highlight button that does nothing forever is what a
    /// discardable answer produces.
    /// </para>
    /// </remarks>
    public long AddHighlight()
    {
        ChapbookException.Check(
            Interop.cb_session_add_highlight(Live(), out long id), nameof(AddHighlight));
        return id;
    }

    /// <summary>
    /// Turn the live selection into a note carrying <paramref name="body"/>.
    /// Throws on the same conditions as <see cref="AddHighlight"/>.
    /// </summary>
    public long AddNote(string body)
    {
        ArgumentNullException.ThrowIfNull(body);
        ChapbookException.Check(
            Interop.cb_session_add_note(Live(), body, out long id), nameof(AddNote));
        return id;
    }

    // ---- Touching them ----

    /// <summary>
    /// The highlight under a point in logical panel coordinates, or
    /// <c>null</c> on a miss — what a click on marked text asks before the
    /// host opens its recolour-or-remove menu.
    /// </summary>
    /// <remarks>
    /// Ask it <b>after <see cref="LinkAt"/> and before the tap zones</b>:
    /// highlights are exact, so a miss falls through to the turn band
    /// naturally.
    /// </remarks>
    public long? HighlightAt(float x, float y)
    {
        Status status = Interop.cb_session_highlight_at(Live(), x, y, out long id);
        if (status == Status.Unavailable)
        {
            return null;
        }
        ChapbookException.Check(status, nameof(HighlightAt));
        return id;
    }

    /// <summary>
    /// Recolour a highlight. <paramref name="color"/> is <c>#rrggbb</c> or
    /// <c>#rrggbbaa</c>; <c>null</c> gives the theme's colour back.
    /// </summary>
    public void SetHighlightColor(long id, string? color) =>
        ChapbookException.Check(
            Interop.cb_session_set_highlight_color(Live(), id, color), nameof(SetHighlightColor));

    /// <summary>
    /// Remove a mark, whatever its kind. The removal reaches the book's
    /// annotation container on the next sync; nothing here is silent.
    /// </summary>
    public void RemoveAnnotation(long id) =>
        ChapbookException.Check(
            Interop.cb_session_remove_annotation(Live(), id), nameof(RemoveAnnotation));

    /// <summary>
    /// Jump to a mark, pushing the return position for
    /// <see cref="ReaderAction.Back"/> the way a followed link does.
    /// Returns whether the reader went anywhere.
    /// </summary>
    public bool GoToAnnotation(long id) =>
        Moved(
            Interop.cb_session_goto_annotation(Live(), id, out byte m), m, nameof(GoToAnnotation));

    /// <inheritdoc cref="GoToAnnotation(long)"/>
    public bool GoTo(Annotation annotation)
    {
        ArgumentNullException.ThrowIfNull(annotation);
        return GoToAnnotation(annotation.Id);
    }
}
