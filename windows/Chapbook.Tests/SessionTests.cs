using Chapbook;
using Xunit;

namespace Chapbook.Tests;

/// <summary>
/// Fixtures and a scratch library, so no test writes where a person's
/// books live.
/// </summary>
internal static class Fixture
{
    /// <summary>
    /// The repository's own fixture directory, found by walking up from
    /// the test assembly rather than by a relative path that depends on
    /// the build layout.
    /// </summary>
    internal static string Dir(string relative)
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !Directory.Exists(Path.Combine(dir.FullName, "fixtures")))
        {
            dir = dir.Parent;
        }
        Assert.NotNull(dir);
        return Path.Combine(dir!.FullName, "fixtures", relative);
    }

    internal static string Book(string relative) => Dir(Path.Combine("epub", relative));

    /// <summary>One of the OPDS wire-format fixtures, as bytes to serve.</summary>
    internal static byte[] Opds(string name) => File.ReadAllBytes(Dir(Path.Combine("opds", name)));

    /// <summary>The checked-in C ABI header, which is the contract.</summary>
    internal static string Header()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !Directory.Exists(Path.Combine(dir.FullName, "crates")))
        {
            dir = dir.Parent;
        }
        Assert.NotNull(dir);
        return Path.Combine(
            dir!.FullName, "crates", "chapbook-ffi", "include", "chapbook.h");
    }

    /// <summary>
    /// A configuration over the repository's four embedded faces and a
    /// scratch library directory.
    /// </summary>
    /// <remarks>
    /// Embedded rather than host fonts, so a machine with an unusual font
    /// set cannot change what a test asserts — and so the tests say the
    /// same thing on a CI runner as on a desk.
    /// </remarks>
    internal static SessionConfiguration Config(string libraryDir) =>
        new SessionConfiguration(FontSource.Embedded(Dir("fonts"), "Crimson Text"))
            .WithLibraryDirectory(libraryDir);

    /// <summary>A directory that exists for the life of one test.</summary>
    internal static string Scratch([System.Runtime.CompilerServices.CallerMemberName] string name = "")
    {
        string dir = Path.Combine(Path.GetTempPath(), "chapbook-dotnet-tests", name);
        if (Directory.Exists(dir))
        {
            Directory.Delete(dir, recursive: true);
        }
        Directory.CreateDirectory(dir);
        return dir;
    }

    internal static readonly PageMetrics Metrics = new(600, 800, 40);

    /// <summary>
    /// Drain background loads until nothing is pending.
    /// </summary>
    /// <remarks>
    /// A freshly opened session restores the reader's place on the loader
    /// thread, so reading the position straight after the metrics are set
    /// can still answer "the beginning". The Rust conformance harness
    /// settles for the same reason before it checks a restart.
    /// </remarks>
    internal static void Settle(Session session, int millis = 5000)
    {
        var deadline = DateTime.UtcNow.AddMilliseconds(millis);
        while (DateTime.UtcNow < deadline)
        {
            session.PollLoaded();
            if (!session.HasPendingLoads)
            {
                return;
            }
            Thread.Sleep(10);
        }
        Assert.Fail("loads never converged");
    }
}

public class EngineTests
{
    [Fact]
    public void TheLoadedLibraryAnswersForItself()
    {
        // If this throws `DllNotFoundException`, the native library is not
        // beside the test assembly: run `windows/build-native.ps1`.
        Assert.True(Engine.AbiVersion >= 1);

        // A header cannot say which artifact was loaded, which is the whole
        // reason this call exists. The default build has all of them.
        Capabilities caps = Engine.Capabilities;
        Assert.True(caps.HasFlag(Capabilities.Library));
        Assert.True(caps.HasFlag(Capabilities.Cbz));
        Assert.True(caps.HasFlag(Capabilities.Pdf));
    }

    [Fact]
    public void TheKeyTableIsTheEnginesAndNotEachHosts()
    {
        Assert.Equal(ReaderAction.NextPage, Engine.DefaultAction(Key.PageDown));
        Assert.Equal(ReaderAction.PrevPage, Engine.DefaultAction(Key.PageUp));

        // The bezel buttons, which on a desktop are a mouse's thumb
        // buttons. A host that had to invent this mapping would invent a
        // different one.
        Assert.Equal(ReaderAction.NextPage, Engine.DefaultAction(Key.TurnNext));
        Assert.Equal(ReaderAction.PrevPage, Engine.DefaultAction(Key.TurnPrev));

        // Characters come from the layout, not from a key code.
        Assert.Equal(ReaderAction.CycleTheme, Engine.DefaultAction('t'));
        Assert.Equal(ReaderAction.NextUnit, Engine.DefaultAction('n'));
        Assert.Equal(ReaderAction.None, Engine.DefaultAction('z'));
    }

    [Fact]
    public void AFailedCallCarriesACodeAndAMessage()
    {
        using var config = new SessionConfiguration(FontSource.Host());
        var error = Assert.Throws<ChapbookException>(
            () => Session.OpenPath(Path.Combine(Path.GetTempPath(), "no-such-book.epub"), config));

        // The code is the contract. The message is not, so this asserts
        // that there *is* one rather than what it says.
        Assert.NotEqual(Status.Ok, error.Status);
        Assert.NotEmpty(error.Message);
    }
}

public class SessionTests
{
    [Fact]
    public void ABookOpensAndPaginates()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        Assert.Equal("The Long Book", session.Title);
        Assert.Equal(BookKind.Epub, session.Kind);
        Assert.True(session.SpineLength > 1);
        Assert.True(session.PageCount > 1);
        Assert.Equal(ReadingDirection.LeftToRight, session.ReadingDirection);

        // The fixture faces loaded. Zero here is the failure that looks
        // like success: a session with no fonts paginates every book to
        // one blank page and reports nothing.
        Assert.True(session.FontFaceCount > 0);
    }

    [Fact]
    public void TurningReportsWhetherItMovedRatherThanLeavingItToBeDerived()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        Position start = session.Position;
        Assert.True(session.NextPage());
        Assert.NotEqual(start, session.Position);

        // Walk forward until the book ends, and check that the crossing
        // into a new unit still counts as a move — comparing page numbers
        // across that boundary reads a success as a failure, which is what
        // this answer exists to prevent.
        int turns = 1;
        bool crossed = false;
        int spine = session.Position.Spine;
        while (session.NextPage())
        {
            turns++;
            if (session.Position.Spine != spine)
            {
                crossed = true;
                spine = session.Position.Spine;
            }
            Assert.True(turns < 10_000, "the book should end");
        }
        Assert.True(crossed);
        Assert.False(session.NextPage());
    }

    [Fact]
    public void TapZonesReadTheBooksDirectionAndTheMiddleCanBeInert()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        // Widths, not boundaries: two bands of three tenths each, leaving
        // four tenths in the middle. Reading these as edges is the mistake
        // that produces a reader with no middle band at all, which is why
        // the binding names them for what they are.
        session.SetTapZones(0.3f, 0.3f, ReaderAction.None);

        Assert.Equal(ReaderAction.PrevPage, session.TapAction(30, 400));
        Assert.Equal(ReaderAction.NextPage, session.TapAction(570, 400));

        // A band bound to nothing answers with nothing, rather than with
        // an action the engine would then refuse.
        Assert.Null(session.TapAction(300, 400));
    }

    [Fact]
    public void AnOutcomeAnswersRepaintAndConsumedSeparately()
    {
        using var session = Session.OpenPath(
            Fixture.Book("minimal.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        Assert.Equal(ActionOutcome.Changed, session.Apply(ReaderAction.CycleTheme));

        // The engine has no chrome, so this is always the host's. It is the
        // one outcome that means "let the platform have this event".
        Assert.Equal(ActionOutcome.Unhandled, session.Apply(ReaderAction.ToggleMenu));

        // Back with an empty trail is the other one, and deliberately so:
        // the bottom of the stack is where the platform's own Back should
        // take over.
        Assert.Equal(ActionOutcome.Unhandled, session.Apply(ReaderAction.Back));
    }

    [Fact]
    public void SettingsRoundTripAndAFontChoiceSurvivesASizeChange()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        ReadingSettings settings = session.Settings with { Theme = Theme.Dark, BaseFontSize = 22 };
        session.SetSettings(settings, SettingsScope.ThisBook);
        Assert.Equal(Theme.Dark, session.Settings.Theme);
        Assert.Equal(22, session.Settings.BaseFontSize);

        Assert.Contains("Crimson Text", session.FontFamilies);
        session.SetFontFamily("Crimson Text", SettingsScope.ThisBook);
        Assert.Equal("Crimson Text", session.FontFamily);

        // The ABI preserves the family across a settings write, so
        // changing the size does not silently discard the typeface.
        session.SetSettings(session.Settings with { BaseFontSize = 18 }, SettingsScope.ThisBook);
        Assert.Equal("Crimson Text", session.FontFamily);

        session.SetFontFamily(null, SettingsScope.ThisBook);
        Assert.Null(session.FontFamily);
    }

    [Fact]
    public void APageRendersAsPremultipliedRgba()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);

        (uint width, uint height) = session.RenderSize();
        Assert.Equal(600u, width);
        Assert.Equal(800u, height);

        (byte[] pixels, _, _) = session.Render();
        Assert.Equal(600 * 800 * 4, pixels.Length);

        // The page is opaque, and its ground is the light theme's white.
        Assert.Equal(255, pixels[3]);
        Assert.Equal(255, pixels[0]);
        Assert.Equal(255, pixels[1]);
        Assert.Equal(255, pixels[2]);

        // Something was drawn: a page of text is not a blank rectangle,
        // and a session with no fonts would produce exactly one.
        Assert.Contains(pixels, b => b != 255);
    }

    [Fact]
    public void TheSepiaThemeProvesTheChannelOrderIsNotSwapped()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        session.SetSettings(session.Settings with { Theme = Theme.Sepia }, SettingsScope.ThisBook);

        (byte[] pixels, _, _) = session.Render();

        // Warm paper reads R > G > B. A channel swap would come back cold
        // blue and every other assertion in this file would still pass —
        // which is why this is the one that has to exist. Same argument the
        // iOS package settles the same way.
        Assert.True(pixels[0] > pixels[1], "red above green");
        Assert.True(pixels[1] > pixels[2], "green above blue");
    }

    [Fact]
    public void APositionSurvivesAClose()
    {
        string library = Fixture.Scratch();
        int spine;
        int page;
        using (var session = Session.OpenPath(Fixture.Book("long.epub"), Fixture.Config(library)))
        {
            session.SetMetrics(Fixture.Metrics);
            session.NextUnit();
            session.NextPage();
            (spine, page) = (session.Position.Spine, session.Position.Page);
            Assert.True(spine > 0);

            // The bookmark. Disposing does not leave one — the C ABI
            // carries no `save_position`, so suspending is how a place is
            // persisted across it, and a host that forgets reopens at the
            // beginning with no error anywhere.
            session.Suspend();
        }

        using (var reopened = Session.OpenPath(Fixture.Book("long.epub"), Fixture.Config(library)))
        {
            reopened.SetMetrics(Fixture.Metrics);
            Fixture.Settle(reopened);

            // The unit is restored immediately; the *page* inside it is
            // not, because an offset cannot become a page until a frame is
            // taken. Laying the unit out is not enough — `PageCount` here
            // still answers page 0 — and rendering is what applies the
            // pending restore. A host that reads the position before it
            // draws sees the top of the chapter, which looks exactly like
            // a bookmark that was never saved.
            Assert.Equal(spine, reopened.Position.Spine);
            _ = reopened.Render();
            Assert.Equal(page, reopened.Position.Page);
        }
    }

    [Fact]
    public void SuspendPersistsAndTheSessionKeepsWorking()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        session.NextPage();

        session.Suspend();

        // The stronger lifecycle call closes the database rather than
        // merely flushing, and the session reopens it on the next access
        // rather than being dead.
        Assert.True(session.NextPage());
        Assert.True(session.PageCount > 0);
    }

    [Fact]
    public void CachesCanBeCappedAndGivenBack()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        session.NextUnit();
        session.NextUnit();

        Assert.True(session.CacheBudget > 0);
        session.ReleaseCaches();

        // Eviction never drops the unit on screen, so the page is still
        // there to draw.
        Assert.True(session.PageCount > 0);
    }
}

public class TextSurfaceTests
{
    [Fact]
    public void ThePageHasLinesWithGeometryAndLocatorRanges()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        // Metrics are not layout, and layout is lazy. Asking for the page
        // count is the cheap way to force it without rendering.
        _ = session.PageCount;

        IReadOnlyList<TextRun>? runs = session.PageTextRuns();
        Assert.NotNull(runs);
        Assert.NotEmpty(runs!);

        foreach (TextRun run in runs!)
        {
            Assert.NotEmpty(run.Text);
            Assert.True(run.Rect.Width > 0 && run.Rect.Height > 0);
            Assert.True(run.LocatorEnd >= run.LocatorStart);
            // Inside the page box, margins included.
            Assert.InRange(run.Rect.X, 0, 600);
            Assert.InRange(run.Rect.Y, 0, 800);
        }
    }

    [Fact]
    public void TheSpeakablePageMapsWordsBackToLocatorSpace()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        _ = session.PageCount;

        SpeakablePage? page = session.SpeakablePage();
        Assert.NotNull(page);
        Assert.NotEmpty(page!.Text);
        Assert.NotEmpty(page.Words);

        // Words are in reading order and never overlap, which is what lets
        // a speech engine's progress be mapped back to the page.
        for (int i = 1; i < page.Words.Count; i++)
        {
            Assert.True(page.Words[i].TextStart >= page.Words[i - 1].TextEnd);
        }

        // Every word indexes the string it came with.
        WordSpan first = page.Words[0];
        Assert.True(first.TextEnd <= page.Text.Length);
        string word = page.Text[(int)first.TextStart..(int)first.TextEnd];
        Assert.NotEmpty(word.Trim());

        // And its locator range answers as geometry — the join that makes
        // an accessibility tree or a speech highlight possible.
        IReadOnlyList<PageRect> rects = session.RangeRects(first.LocatorStart, first.LocatorEnd);
        Assert.NotEmpty(rects);
        Assert.True(rects[0].Width > 0);
    }

    [Fact]
    public void APointAnswersWithTheWordUnderIt()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        _ = session.PageCount;

        SpeakablePage page = session.SpeakablePage()!;
        WordSpan target = page.Words[3];
        PageRect rect = session.RangeRects(target.LocatorStart, target.LocatorEnd)[0];

        (uint Start, uint End)? hit = session.WordAt(
            rect.X + rect.Width / 2, rect.Y + rect.Height / 2);

        Assert.NotNull(hit);
        Assert.Equal(target.LocatorStart, hit!.Value.Start);
        Assert.Equal(target.LocatorEnd, hit.Value.End);
    }

    [Fact]
    public void AComicHasNoTextSurfaceAndSaysSoWithoutFailing()
    {
        using var session = Session.OpenPath(
            Fixture.Dir(Path.Combine("cbz", "minimal.cbz")), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        _ = session.PageCount;

        Assert.Equal(BookKind.Comic, session.Kind);

        // An image book has nothing to read aloud. That is an empty answer,
        // not an error — the distinction a host needs in order to decide
        // whether to show an accessibility tree at all.
        SpeakablePage? page = session.SpeakablePage();
        if (page is not null)
        {
            Assert.Empty(page.Words);
        }
    }
}

public class SessionEventTests
{
    [Fact]
    public void ThePositionMovingIsReportedAsAnEvent()
    {
        using var session = Session.OpenPath(
            Fixture.Book("long.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        _ = session.Render();
        session.DrainEvents();

        session.NextUnit();
        session.NextPage();

        IReadOnlyList<SessionEvent> events = session.DrainEvents();
        SessionEvent moved = Assert.Single(
            events, e => e.Kind == SessionEventKind.PositionChanged);

        // Coalesced on the engine's side: two moves, one report. A host
        // that drains rarely still learns where the reader ended up, which
        // is why this is a queue and not a callback per turn.
        Assert.Equal(session.Position.Spine, moved.Spine);
        Assert.Equal(session.Position.Page, moved.Page);
    }

    [Fact]
    public void ReachingTheEndOfTheBookIsAnEvent()
    {
        using var session = Session.OpenPath(
            Fixture.Book("minimal.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        _ = session.Render();

        bool finished = false;
        for (int turn = 0; turn < 500 && !finished; turn++)
        {
            bool moved = session.NextPage();
            _ = session.Render();
            finished = session.DrainEvents()
                .Any(e => e.Kind == SessionEventKind.BookFinished);
            if (!moved)
            {
                break;
            }
        }
        Assert.True(finished, "the last page of the last unit should report itself");
    }

    [Fact]
    public void AnEmptyQueueIsNotAFailure()
    {
        using var session = Session.OpenPath(
            Fixture.Book("minimal.epub"), Fixture.Config(Fixture.Scratch()));
        session.SetMetrics(Fixture.Metrics);
        session.DrainEvents();

        // The ordinary answer. An ABI that reported this as an error would
        // make every idle drain look like a problem.
        Assert.Null(session.NextEvent());
        Assert.Empty(session.DrainEvents());
    }
}
