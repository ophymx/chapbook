namespace Chapbook;

/// <summary>
/// Where the reader is, as a place in the text: a spine unit and a
/// character offset into it.
/// </summary>
/// <remarks>
/// <para>
/// The <b>durable</b> half of the pair, and the one to save. The library
/// stores this, marks anchor to it and sync carries it, while
/// <see cref="Session.Position"/> counts pages in the <i>current</i>
/// pagination — so a position saved at one font size means somewhere else
/// at another, and a locator still means this passage.
/// </para>
/// <para>
/// The offset is where the current page begins, so it lands on a page
/// boundary rather than on the exact character last looked at, and a
/// reflow can move it within the same passage. What holds across both is
/// the round trip: hand one back to <see cref="Session.GoTo(Locator)"/>
/// and the reader is where they were.
/// </para>
/// </remarks>
public readonly record struct Locator(int Spine, uint Offset);

/// <summary>
/// One entry of the table of contents, flattened. <see cref="Depth"/>
/// carries the nesting a menu indents by.
/// </summary>
/// <param name="Index">
/// The entry's index in this book's contents, which is what
/// <see cref="Session.GoTo(TocEntry)"/> takes. Labels repeat across a
/// book; indices do not.
/// </param>
/// <param name="Depth">0 for a top-level entry, 1 for its children.</param>
/// <param name="Spine">
/// The spine unit it points at, or <c>null</c> for an entry that named
/// none when the contents were read — typically a heading that links
/// nowhere. Not the same question as "can I jump to it": the jump also
/// resolves an entry's href, so <c>null</c> here and a successful jump is
/// an ordinary pairing. Show them all and let the jump answer.
/// </param>
/// <param name="PointsWithinUnit">
/// Whether it points inside its unit rather than at the start. Nothing to
/// act on; the jump handles it either way.
/// </param>
public sealed record TocEntry(
    int Index, string Label, int Depth, int? Spine, bool PointsWithinUnit);

// Getting somewhere on purpose: the contents, the durable locator, the
// jumps, and the links a reader clicks.
//
// **The jumps push the return position for `ReaderAction.Back`; the
// skips do not.** Pushing are `GoTo` in each of its forms, `GoToAnchor`,
// `GoToAnnotation` and `FollowLink`. Not pushing are the page turns and
// `NextUnit`/`PrevUnit` — a chapter skip is a reader walking through the
// book, not a departure from somewhere they meant to come back to, and
// if it pushed, "back" would spend itself undoing navigation the reader
// did on purpose. `CanGoBack` is what greys out the button.
public sealed partial class Session
{
    // ---- The contents ----

    /// <summary>
    /// The book's contents in reading order. Empty for a book with none,
    /// which is ordinary — a comic has no contents.
    /// </summary>
    public IReadOnlyList<TocEntry> TableOfContents()
    {
        ChapbookException.Check(
            Interop.cb_session_toc_count(Live(), out nuint count), nameof(TableOfContents));
        var entries = new List<TocEntry>((int)count);
        for (nuint i = 0; i < count; i++)
        {
            nuint index = i;
            ChapbookException.Check(
                Interop.cb_session_toc_entry(Live(), index, out NativeTocEntry entry),
                nameof(TableOfContents));
            string label = Strings.Read((byte[]? b, nuint c, out nuint n) =>
                Interop.cb_session_toc_label(Live(), index, b, c, out n),
                nameof(TableOfContents)) ?? string.Empty;
            entries.Add(new TocEntry(
                (int)index,
                label,
                (int)entry.Depth,
                entry.HasSpine != 0 ? (int)entry.Spine : null,
                entry.HasFragment != 0));
        }
        return entries;
    }

    /// <summary>
    /// Jump to a contents entry. <c>false</c> for one that resolves to no
    /// unit by either its spine index or its href — a section heading —
    /// which is not an error. Throws only if the entry is no longer there,
    /// which means the contents were re-read since.
    /// </summary>
    public bool GoTo(TocEntry entry)
    {
        ArgumentNullException.ThrowIfNull(entry);
        return Moved(
            Interop.cb_session_goto_toc(Live(), (nuint)entry.Index, out byte m), m, nameof(GoTo));
    }

    // ---- Locators ----

    /// <summary>Where the reader is now.</summary>
    /// <remarks>
    /// Before the first <see cref="SetMetrics"/> there is no pagination
    /// to ask, and this answers offset 0 rather than refusing — so a host
    /// that persists a place on an early suspend saves the top of the unit
    /// and gets no signal that it did. Set metrics first, or do not save
    /// what an unlaid-out session reports.
    /// </remarks>
    public Locator Locator
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_locator(Live(), out nuint spine, out uint offset),
                nameof(Locator));
            return new Locator((int)spine, offset);
        }
    }

    /// <summary>
    /// Jump to a locator — a place saved earlier, a search hit, a position
    /// another device reached. <c>false</c> for a spine index the book
    /// does not have; an offset past the unit's text lands at its end
    /// rather than failing.
    /// </summary>
    public bool GoTo(Locator locator) =>
        Moved(
            Interop.cb_session_goto(Live(), (nuint)locator.Spine, locator.Offset, out byte m),
            m, nameof(GoTo));

    /// <summary>
    /// Jump to an element id within a unit — a footnote, a
    /// cross-reference. A fragment the unit does not carry lands at the
    /// unit's start rather than failing, so <c>false</c> means only that
    /// the book has no such spine index.
    /// </summary>
    public bool GoToAnchor(int spine, string fragment)
    {
        ArgumentNullException.ThrowIfNull(fragment);
        return Moved(
            Interop.cb_session_goto_anchor(Live(), (nuint)spine, fragment, out byte m),
            m, nameof(GoToAnchor));
    }

    // ---- The back trail ----

    /// <summary>
    /// Whether <see cref="ReaderAction.Back"/> has anywhere to return to —
    /// a 64-deep stack that jumps push and page turns do not. The question
    /// a back button's enabled state asks; applying the action is the
    /// answer.
    /// </summary>
    public bool CanGoBack
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_can_go_back(Live(), out byte can), nameof(CanGoBack));
            return can != 0;
        }
    }

    // ---- Links ----

    /// <summary>
    /// The link under a point, as the href the book wrote, or <c>null</c>
    /// when the point is not on one.
    /// </summary>
    /// <remarks>
    /// The point is in logical panel coordinates, like every hit test
    /// here. <b>Ask this before starting a selection and before the tap
    /// zones</b>: links are exact, so a miss falls through naturally, and
    /// asking in the other order lets the turn band swallow every link in
    /// the outer thirds of the page — which reads as "links don't work in
    /// this app" rather than as a precedence bug.
    /// </remarks>
    public string? LinkAt(float x, float y) =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_session_link_at(Live(), x, y, b, c, out n), nameof(LinkAt));

    /// <summary>
    /// Follow an href — one <see cref="LinkAt"/> answered, or a contents
    /// entry's. <c>false</c> for anything that is not a reading position:
    /// an external <c>http(s)</c> link is the host's to open in a browser,
    /// and that answer is the host's opportunity rather than a failure.
    /// </summary>
    public bool FollowLink(string href)
    {
        ArgumentNullException.ThrowIfNull(href);
        return Moved(
            Interop.cb_session_follow_link(Live(), href, out byte m), m, nameof(FollowLink));
    }
}
