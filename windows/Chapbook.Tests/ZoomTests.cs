using Chapbook;
using Xunit;

namespace Chapbook.Tests;

/// <summary>
/// The pinch, across the boundary: refused on prose, honoured on a comic,
/// pan falling through at fit.
/// </summary>
public class ZoomTests
{
    [Fact]
    public void ProseRefusesToZoomBecauseTheGestureMeansFontSizeThere()
    {
        using var session = Session.OpenPath(
            Fixture.Book("minimal.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        Assert.False(session.SetPageZoom(2.0f, 100, 100));
        Assert.Equal(1.0f, session.PageZoom);
    }

    [Fact]
    public void AComicZoomsAndPansOnceItsPageHasLanded()
    {
        using var session = Session.OpenPath(
            Fixture.Dir(Path.Combine("cbz", "minimal.cbz")), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        Fixture.Settle(session);

        // Pan at fit falls through, so a drag can mean a swipe turn.
        Assert.False(session.PanPage(-30, 0));

        Assert.True(session.SetPageZoom(2.0f, 300, 400));
        Assert.Equal(2.0f, session.PageZoom);
        Assert.True(session.PanPage(-30, -10), "a zoomed page pans");
        (float x, float y) = session.PagePan;
        Assert.True(x < 0 || y < 0, "the pan moved off origin");

        // 1.0 is fit, and returns there.
        session.SetPageZoom(1.0f, 300, 400);
        Assert.Equal(1.0f, session.PageZoom);
    }
}
