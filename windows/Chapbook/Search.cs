using System.Text;

namespace Chapbook;

/// <summary>
/// One search hit: where it is in the book, and enough context to draw a
/// results row.
/// </summary>
/// <param name="Spine">The unit the match is in.</param>
/// <param name="Start">
/// Locator offset of the match's first character. With <paramref name="End"/>,
/// the range to hand <see cref="Session.SelectRange(uint, uint)"/> after
/// jumping, which is how a hit gets painted on the page.
/// </param>
/// <param name="End">Just past the match's last character, in locator space.</param>
/// <param name="Context">
/// The match with a little text either side, whitespace collapsed.
/// </param>
/// <param name="MatchStart">
/// Where the match sits within <paramref name="Context"/>, so a results
/// list can embolden the matched words rather than the whole line.
/// <b>Unicode scalar offsets, not UTF-16 indices.</b> The engine counts
/// in scalars and a .NET <c>string</c> indexes code units, so walk
/// <see cref="string.EnumerateRunes"/> — slicing by <c>char</c> gets the
/// right answer until the line holds an emoji, and then quietly emboldens
/// the wrong words.
/// </param>
/// <param name="MatchEnd">Just past the match, in the same space.</param>
public sealed record SearchHit(
    int Spine, uint Start, uint End, string Context, uint MatchStart, uint MatchEnd)
{
    /// <summary>Where to jump before selecting: the hit's own start.</summary>
    public Locator Locator => new(Spine, Start);

    /// <summary>
    /// The matched words, cut from <see cref="Context"/> in the engine's
    /// own units — the slice a results row emboldens.
    /// </summary>
    public string Matched
    {
        get
        {
            var text = new StringBuilder();
            uint at = 0;
            foreach (Rune rune in Context.EnumerateRunes())
            {
                if (at >= MatchEnd)
                {
                    break;
                }
                if (at >= MatchStart)
                {
                    text.Append(rune.ToString());
                }
                at++;
            }
            return text.ToString();
        }
    }
}

// Finding words in the book. Two entry points with the same result type:
// the whole spine at once, and one unit at a time for a host that wants
// results as they arrive.
public sealed partial class Session
{
    /// <summary>
    /// Search the whole book, keeping at most <paramref name="limit"/>
    /// hits — 0 for the engine's own sane cap.
    /// </summary>
    /// <remarks>
    /// <b>Blocking and potentially slow</b>: it lays nothing out, but it
    /// reads and folds every unit's text. A responsive search box runs this
    /// off the UI thread, or walks units itself with
    /// <see cref="SearchUnit"/>. The engine holds the hits until the next
    /// search or the session's close, and this reads them all out before
    /// returning, so the list outlives both.
    /// </remarks>
    public IReadOnlyList<SearchHit> Search(string query, int limit = 0)
    {
        ArgumentNullException.ThrowIfNull(query);
        ChapbookException.Check(
            Interop.cb_session_search(Live(), query, (nuint)limit, out nuint count),
            nameof(Search));
        return Hits(count, nameof(Search));
    }

    /// <summary>
    /// Search one unit — the worker-drivable half, for results as they
    /// arrive. Replaces whatever the last search left, exactly as
    /// <see cref="Search"/> does.
    /// </summary>
    public IReadOnlyList<SearchHit> SearchUnit(int spine, string query)
    {
        ArgumentNullException.ThrowIfNull(query);
        ChapbookException.Check(
            Interop.cb_session_search_unit(Live(), (nuint)spine, query, out nuint count),
            nameof(SearchUnit));
        return Hits(count, nameof(SearchUnit));
    }

    private IReadOnlyList<SearchHit> Hits(nuint count, string call)
    {
        var hits = new List<SearchHit>((int)count);
        for (nuint i = 0; i < count; i++)
        {
            nuint index = i;
            ChapbookException.Check(
                Interop.cb_session_search_hit(Live(), index, out NativeSearchHit hit), call);
            string context = Strings.Read((byte[]? b, nuint c, out nuint n) =>
                Interop.cb_session_search_context(Live(), index, b, c, out n), call)
                ?? string.Empty;
            hits.Add(new SearchHit(
                (int)hit.Spine, hit.Start, hit.End, context, hit.MatchStart, hit.MatchEnd));
        }
        return hits;
    }
}
