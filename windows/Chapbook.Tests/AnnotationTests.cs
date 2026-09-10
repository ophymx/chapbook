using Chapbook;
using Xunit;

namespace Chapbook.Tests;

/// <summary>
/// A mark's whole life across the boundary: selected, kept, found under
/// a click, recoloured, listed, jumped to, removed.
/// </summary>
public class AnnotationTests
{
    /// <summary>
    /// Where the text sits depends on the fixture fonts, so sweep for a
    /// word rather than knowing a coordinate.
    /// </summary>
    private static void SelectAWord(Session session)
    {
        for (int y = 40; y < 760; y += 20)
        {
            for (int x = 40; x < 560; x += 20)
            {
                if (session.SelectWordAt(x, y))
                {
                    return;
                }
            }
        }
        Assert.Fail("a page of text has a word to select");
    }

    [Fact]
    public void AHighlightLivesItsWholeLifeAcrossTheBoundary()
    {
        using var session = Session.OpenPath(
            Fixture.Book("minimal.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        SelectAWord(session);
        LocatorRange word = session.SelectedRange ?? throw new Xunit.Sdk.XunitException("no selection");
        Assert.True(word.End > word.Start, "a word is a non-empty range");
        string text = session.SelectedText ?? throw new Xunit.Sdk.XunitException("no text");
        Assert.NotEmpty(text.Trim());

        // Grow the selection by exact range — the adjusted-handle move.
        var grown = new LocatorRange(word.Start, word.End + 4);
        session.SelectRange(grown);

        long id = session.AddHighlight();
        Assert.True(id > 0);

        // The highlight replaces the selection, and saying so is the
        // host's move — the paint order is explicit.
        session.ClearSelection();
        Assert.Null(session.SelectedRange);
        Assert.Null(session.SelectedText);

        // The click that opens the recolour menu — aimed at the
        // highlight's own ink, since the hit test is exact where the word
        // sweep above was allowed to snap.
        PageRect rect = session.RangeRects(grown.Start, grown.End)[0];
        Assert.Equal(id, session.HighlightAt(rect.X + rect.Width / 2, rect.Y + rect.Height / 2));
        Assert.Null(session.HighlightAt(2, 2));

        // Listed, recoloured, read back.
        session.SetHighlightColor(id, "#ffcc00");
        Annotation mark = Assert.Single(session.Annotations());
        Assert.Equal(id, mark.Id);
        Assert.Equal(AnnotationKind.Highlight, mark.Kind);
        Assert.Equal("#ffcc00", mark.Color);
        Assert.Contains(text.Trim(), mark.Text);
        Assert.InRange(mark.Progression, 0.0, 1.0);

        // Back to the theme's colour.
        session.SetHighlightColor(id, null);
        Assert.Null(Assert.Single(session.Annotations()).Color);

        // Jump to it from somewhere else, then remove it.
        session.NextPage();
        session.GoTo(mark);
        Assert.True(session.CanGoBack);
        session.RemoveAnnotation(id);
        Assert.Empty(session.Annotations());
    }

    [Fact]
    public void ABookmarkIsAPointAndANoteCarriesItsWords()
    {
        using var session = Session.OpenPath(
            Fixture.Book("minimal.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        long bookmark = session.AddBookmark();
        Annotation point = Assert.Single(session.Annotations());
        Assert.Equal(bookmark, point.Id);
        Assert.Equal(AnnotationKind.Bookmark, point.Kind);
        Assert.Null(point.Text);
        Assert.Null(point.Color);

        // A note needs a selection, and refuses by code without one.
        var error = Assert.Throws<ChapbookException>(() => session.AddNote("no selection"));
        Assert.Equal(Status.Unavailable, error.Status);

        SelectAWord(session);
        long note = session.AddNote("worth remembering");
        session.ClearSelection();

        // Ordered by progression, ids stable, the note's body on the row.
        IReadOnlyList<Annotation> marks = session.Annotations();
        Assert.Equal(2, marks.Count);
        Annotation kept = Assert.Single(marks, m => m.Id == note);
        Assert.Equal(AnnotationKind.Note, kept.Kind);
        Assert.Equal("worth remembering", kept.Text);
    }
}
