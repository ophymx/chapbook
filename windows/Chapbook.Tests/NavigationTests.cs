using Chapbook;
using Xunit;

namespace Chapbook.Tests;

/// <summary>
/// Contents, search, and the locator: the three ways a reader goes
/// somewhere on purpose, spelled the way a host writes them.
/// </summary>
public class NavigationTests
{
    [Fact]
    public void TheContentsAreFlatAndAJumpLandsInTheEntrysUnit()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        IReadOnlyList<TocEntry> contents = session.TableOfContents();
        Assert.True(contents.Count > 1, "long.epub has contents");
        Assert.Equal(0, contents[0].Depth);
        Assert.Equal(0, contents[0].Index);
        Assert.NotEmpty(contents[0].Label.Trim());

        // Jump to the last entry and land in its unit.
        TocEntry last = contents[^1];
        bool moved = session.GoTo(last);
        if (last.Spine is { } spine)
        {
            Assert.True(moved, "an entry that points somewhere moves the reader");
            Assert.Equal(spine, session.Position.Spine);
        }

        // An entry from a re-read that is no longer there is refused by
        // name, not by crashing.
        var stale = new TocEntry(contents.Count, "nowhere", 0, null, false);
        var error = Assert.Throws<ChapbookException>(() => session.GoTo(stale));
        Assert.Equal(Status.InvalidArgument, error.Status);
    }

    [Fact]
    public void ALocatorRoundTripsAndAJumpFillsTheBackTrail()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        // Page turns do not push. Back is greyed out.
        session.NextPage();
        Assert.False(session.CanGoBack);

        // Somewhere worth returning to, then away, then back exactly.
        session.GoTo(session.TableOfContents()[^1]);
        Locator saved = session.Locator;

        Assert.True(session.GoTo(new Locator(0, 0)));
        Assert.True(session.GoTo(saved));
        Locator back = session.Locator;
        Assert.Equal(saved.Spine, back.Spine);
        Assert.True(back.Offset <= saved.Offset, "lands at or before the offset, never past");

        Assert.True(session.CanGoBack, "jumping is what fills the back stack");

        // A spine index the book does not have is a refusal, not a crash;
        // an anchor the unit lacks lands at its start rather than failing.
        Assert.False(session.GoTo(new Locator(9999, 0)));
        Assert.False(session.GoToAnchor(9999, "anywhere"));
        Assert.True(session.GoToAnchor(0, "no-such-id"));
    }

    [Fact]
    public void SearchHonorsTheLimitAndAHitCanBePaintedOnThePage()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        IReadOnlyList<SearchHit> hits = session.Search("the", limit: 10);
        Assert.InRange(hits.Count, 1, 10);

        SearchHit hit = hits[0];
        Assert.True(hit.End > hit.Start, "a match is a non-empty range");
        Assert.NotEmpty(hit.Context);
        // The match range indexes the context, so a results row can
        // embolden it — and it is counted in the engine's units, which
        // `Matched` walks correctly where a `Substring` would not.
        Assert.Equal("the", hit.Matched.ToLowerInvariant());

        // Going to a hit and painting it is the flow a search box runs.
        Assert.True(session.GoTo(hit.Locator));
        session.SelectRange(hit.Start, hit.End);
        Assert.NotNull(session.SelectedRange);
        Assert.Equal(new LocatorRange(hit.Start, hit.End), session.SelectedRange);

        // Per-unit search is the worker-drivable half, and stays in its unit.
        foreach (SearchHit unitHit in session.SearchUnit(0, "the"))
        {
            Assert.Equal(0, unitHit.Spine);
        }
        var error = Assert.Throws<ChapbookException>(() => session.SearchUnit(9999, "the"));
        Assert.Equal(Status.InvalidArgument, error.Status);
    }

    [Fact]
    public void AnExternalLinkIsTheHostsToOpenAndAMissIsNull()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        _ = session.PageCount;

        // The engine does not browse; it says so quietly rather than
        // failing, because that answer is the host's opportunity.
        Assert.False(session.FollowLink("https://example.com/elsewhere"));

        // The margin is not a link.
        Assert.Null(session.LinkAt(2, 2));
    }
}
