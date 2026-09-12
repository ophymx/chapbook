using System.Runtime.InteropServices;

namespace Chapbook;

/// <summary>Where the reader is: which spine unit, and which page in it.</summary>
/// <remarks>
/// The pair, never the page alone. Turning forward off the end of a unit
/// crosses into the next one by resetting the page to zero, so a host that
/// compares page numbers across a turn reads a successful move as a failed
/// one — and since most books open on a single-page cover, it reads that
/// on the very first turn. Use the <c>moved</c> answer the navigation calls
/// return, which is why they return one.
/// </remarks>
public readonly record struct Position(int Spine, int Page);

/// <summary>How big a page is, in the units text flows in.</summary>
/// <param name="Width">Logical pixels, margins included.</param>
/// <param name="Height">Logical pixels, margins included.</param>
/// <param name="DpiScale">
/// Device pixels per logical pixel. Passed to the rasterizer and *not* a
/// layout input, so the same book paginates identically at 100% and 200%.
/// </param>
/// <param name="Rotation">
/// Applied on the way to the buffer, so turning the panel does not
/// repaginate the book.
/// </param>
public readonly record struct PageMetrics(
    float Width,
    float Height,
    float MarginTop,
    float MarginRight,
    float MarginBottom,
    float MarginLeft,
    float DpiScale = 1.0f,
    Rotation Rotation = Rotation.None)
{
    /// <summary>The common case: one margin on all four sides.</summary>
    public PageMetrics(float width, float height, float margin, float dpiScale = 1.0f)
        : this(width, height, margin, margin, margin, margin, dpiScale)
    {
    }

    internal NativeMetrics ToNative() => new()
    {
        Width = Width,
        Height = Height,
        MarginTop = MarginTop,
        MarginRight = MarginRight,
        MarginBottom = MarginBottom,
        MarginLeft = MarginLeft,
        DpiScale = DpiScale,
        Rotation = Rotation,
    };
}

/// <summary>The reader's typography and colour choices.</summary>
/// <remarks>
/// <see cref="BaseFontSize"/> and <see cref="LineHeight"/> lose to a
/// publisher that specifies; a font family chosen through
/// <see cref="Session.SetFontFamily"/> beats one, which is deliberate and
/// documented there.
/// </remarks>
public readonly record struct ReadingSettings(
    float BaseFontSize,
    float LineHeight,
    bool Justify,
    bool PublisherStyles,
    Theme Theme)
{
    internal static ReadingSettings From(NativeSettings s) =>
        new(s.BaseFontPx, s.LineHeight, s.Justify != 0, s.PublisherStyles != 0, s.Theme);

    internal NativeSettings ToNative() => new()
    {
        BaseFontPx = BaseFontSize,
        LineHeight = LineHeight,
        Justify = Justify ? (byte)1 : (byte)0,
        PublisherStyles = PublisherStyles ? (byte)1 : (byte)0,
        Theme = Theme,
    };
}

/// <summary>
/// Something the session wants the host to know that is not "repaint".
/// </summary>
/// <param name="Kind">Which of the four this is.</param>
/// <param name="Spine">The unit, for the two unit events and the position.</param>
/// <param name="Page">
/// The page, for <see cref="SessionEventKind.PositionChanged"/>; zero
/// otherwise.
/// </param>
/// <param name="Message">
/// A unit failure's reason, for a person to read. Free to change, so do
/// not match on it; <c>null</c> for every other kind.
/// </param>
public readonly record struct SessionEvent(
    SessionEventKind Kind, int Spine, int Page, string? Message);

/// <summary>
/// One open book, and everything that follows from it: layout, the reading
/// position, settings, the page's text, and the shelf row it belongs to.
/// </summary>
/// <remarks>
/// <para>
/// <b>A session may be moved between threads and must never be used from
/// two at once.</b> That is the ABI's rule and .NET has no way to check
/// it — Swift 6 can, through region isolation, and this binding cannot,
/// which is worth knowing rather than discovering. Own a session from one
/// place: a view model, a window, an actor-shaped queue. A static field is
/// the shape that breaks it, because a static is reachable from
/// everywhere.
/// </para>
/// <para>
/// <b>Disposing blocks</b> until the loader thread finishes whatever it
/// was fetching, which on a cold comic page over a slow network is
/// seconds. That is deliberate: the worker holds the publication, and
/// returning sooner would hand a host back control while its own transport
/// was still in use on a thread it cannot see. If you are being torn down
/// in a hurry, call <see cref="Suspend"/> — it persists the position and
/// lets go without waiting on the network.
/// </para>
/// </remarks>
public sealed partial class Session : IDisposable
{
    private nint _handle;
    private GCHandle _waker;

    private Session(nint handle) => _handle = handle;

    private nint Live() =>
        _handle != 0 ? _handle : throw new ObjectDisposedException(nameof(Session));

    // ---- Opening ----

    /// <summary>
    /// Open a book from a path — <c>.epub</c>, <c>.cbz</c> or <c>.pdf</c>,
    /// dispatched on what the bytes say rather than on the extension.
    /// </summary>
    /// <remarks>
    /// The configuration is consumed whether this succeeds or fails.
    /// Opening also imports or matches the book in the library, which is
    /// how a position has somewhere to be saved; <see cref="BookId"/> says
    /// which row that became.
    /// </remarks>
    public static Session OpenPath(string path, SessionConfiguration configuration)
    {
        ArgumentNullException.ThrowIfNull(configuration);
        return Opened(Interop.cb_session_open_path(path, configuration.Consume()), path);
    }

    /// <summary>
    /// Open from bytes already in memory — a stream a host downloaded, a
    /// resource it embedded.
    /// </summary>
    /// <remarks>
    /// A book opened this way is *adopted* rather than imported: recorded
    /// under a fingerprint of its bytes, never copied, so its position and
    /// annotations persist with no path ever crossing. Holding the way back
    /// to the file is the host's half of custody.
    /// </remarks>
    public static Session OpenBytes(
        byte[] bytes, SessionConfiguration configuration, BookFormat format = BookFormat.Guess)
    {
        ArgumentNullException.ThrowIfNull(bytes);
        ArgumentNullException.ThrowIfNull(configuration);
        return Opened(
            Interop.cb_session_open_bytes(
                bytes, (nuint)bytes.Length, format, configuration.Consume()),
            "bytes");
    }

    /// <summary>Open an OPDS catalogue URL as a page-streamed comic.</summary>
    public static Session OpenUrl(string url, SessionConfiguration configuration)
    {
        ArgumentNullException.ThrowIfNull(configuration);
        return Opened(Interop.cb_session_open_url(url, configuration.Consume()), url);
    }

    private static Session Opened(nint handle, string what)
    {
        if (handle == 0)
        {
            throw ChapbookException.From(Status.BookOpen, $"opening {what}");
        }
        return new Session(handle);
    }

    // ---- Metrics ----

    /// <summary>
    /// Say how big a page is. Nothing else here produces anything until
    /// this has been called, because the engine does not know what a page
    /// is until it is told.
    /// </summary>
    /// <remarks>
    /// Call it again on every resize and scale change. It is cheap when
    /// nothing changed, it keeps the reader's place when the page box did,
    /// and it skips the relayout entirely when only the rotation moved.
    /// </remarks>
    public void SetMetrics(PageMetrics metrics) =>
        ChapbookException.Check(
            Interop.cb_session_set_metrics(Live(), metrics.ToNative()), nameof(SetMetrics));

    // ---- Where we are ----

    /// <summary>The book's title, or <c>null</c> if it declares none.</summary>
    public string? Title =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_session_title(Live(), b, c, out n), nameof(Title));

    /// <summary>What kind of publication this is.</summary>
    public BookKind Kind
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_book_kind(Live(), out BookKind kind), nameof(Kind));
            return kind;
        }
    }

    /// <summary>Which edge the book reads from.</summary>
    public ReadingDirection ReadingDirection
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_reading_direction(Live(), out ReadingDirection d),
                nameof(ReadingDirection));
            return d;
        }
    }

    /// <summary>Where the reader is.</summary>
    /// <remarks>
    /// <b>A freshly opened session has not finished arriving here.</b> The
    /// unit is restored as soon as the loads settle, but the page inside it
    /// is not: an offset cannot become a page until a frame has been taken,
    /// and laying the unit out is not taking one. So a reopened session
    /// answers with the top of the restored chapter until it has been
    /// rendered once — which reads as a bookmark that was never saved. Draw
    /// first, then ask.
    /// </remarks>
    public Position Position
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_position(Live(), out NativePosition p), nameof(Position));
            return new Position((int)p.Spine, (int)p.Page);
        }
    }

    /// <summary>
    /// How many units the spine holds. Never compacted — a dangling idref
    /// keeps its slot, because indices are locator identity.
    /// </summary>
    public int SpineLength
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_spine_len(Live(), out nuint len), nameof(SpineLength));
            return (int)len;
        }
    }

    /// <summary>
    /// How many pages the current unit holds at the current metrics. Lays
    /// the unit out if it has not been, so it is not free.
    /// </summary>
    public int PageCount
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_page_count(Live(), out nuint count), nameof(PageCount));
            return (int)count;
        }
    }

    /// <summary>
    /// The shelf row this book became when it was opened, or <c>null</c> if
    /// it never reached the library.
    /// </summary>
    public long? BookId
    {
        get
        {
            Status status = Interop.cb_session_book_id(Live(), out long book);
            if (status == Status.Unavailable)
            {
                return null;
            }
            ChapbookException.Check(status, nameof(BookId));
            return book;
        }
    }

    // ---- Navigation ----

    /// <summary>Forward one page. Returns whether the position moved.</summary>
    public bool NextPage() =>
        Moved(Interop.cb_session_next_page(Live(), out byte m), m, nameof(NextPage));

    /// <summary>Back one page. Returns whether the position moved.</summary>
    public bool PrevPage() =>
        Moved(Interop.cb_session_prev_page(Live(), out byte m), m, nameof(PrevPage));

    /// <summary>Forward one unit, landing on its first page.</summary>
    public bool NextUnit() =>
        Moved(Interop.cb_session_next_unit(Live(), out byte m), m, nameof(NextUnit));

    /// <summary>Back one unit.</summary>
    public bool PrevUnit() =>
        Moved(Interop.cb_session_prev_unit(Live(), out byte m), m, nameof(PrevUnit));

    private static bool Moved(Status status, byte moved, string call)
    {
        ChapbookException.Check(status, call);
        return moved != 0;
    }

    // ---- Input ----

    /// <summary>
    /// Set how wide the previous- and next-page bands are, and what a
    /// press between them does.
    /// </summary>
    /// <param name="previousWidth">
    /// The previous-page band's width as a fraction of the page. Thirds
    /// are 0.33.
    /// </param>
    /// <param name="nextWidth">The next-page band's width, likewise.</param>
    /// <remarks>
    /// <para>
    /// <b>Widths, not boundaries.</b> Passing 0.3 and 0.7 does not leave a
    /// middle band — it makes the next-page band seven tenths of the page,
    /// overlapping the other, and overlaps resolve to the previous-page
    /// side. The two numbers are independent widths measured inward from
    /// each edge, which is why they are named this way here: the ABI's own
    /// parameters are <c>left</c> and <c>right</c>, and reading those as
    /// edges is a mistake that produces a working reader with one band
    /// missing.
    /// </para>
    /// <para>
    /// The reading direction is deliberately not a parameter: it is the
    /// book's, and the engine re-reads it, so a host cannot flip a book by
    /// configuring it. Give a band <see cref="ReaderAction.None"/> when it
    /// should do nothing — a host with no menu should not leave the middle
    /// bound to <see cref="ReaderAction.ToggleMenu"/>, which the engine
    /// would only decline.
    /// </para>
    /// <para>
    /// Only the middle band's action is a parameter. The outer two turn
    /// pages, always — which is the policy the engine is willing to hold,
    /// and the reason a host cannot accidentally bind the next-page band
    /// to something else.
    /// </para>
    /// <para>
    /// Until this is called a session has thirds, with the middle opening
    /// a menu.
    /// </para>
    /// </remarks>
    public void SetTapZones(
        float previousWidth, float nextWidth,
        ReaderAction middleAction = ReaderAction.ToggleMenu) =>
        ChapbookException.Check(
            Interop.cb_session_set_tap_zones(Live(), previousWidth, nextWidth, middleAction),
            nameof(SetTapZones));

    /// <summary>
    /// What a press at a point means, or <c>null</c> for a band bound to
    /// nothing.
    /// </summary>
    /// <remarks>
    /// The point is in the same logical units <see cref="SetMetrics"/> was
    /// given, and the engine undoes any rotation itself. Forwarding raw
    /// device pixels is the silent failure here: on a 200% display every
    /// press lands in the far band and always means "next page".
    /// </remarks>
    public ReaderAction? TapAction(float x, float y)
    {
        ChapbookException.Check(
            Interop.cb_session_tap_action(Live(), x, y, out ReaderAction action),
            nameof(TapAction));
        return action == ReaderAction.None ? null : action;
    }

    /// <summary>
    /// Apply an action. The outcome answers two questions — whether to
    /// repaint and whether the reader consumed the event — and a host
    /// needs both.
    /// </summary>
    public ActionOutcome Apply(ReaderAction action)
    {
        ChapbookException.Check(
            Interop.cb_session_apply(Live(), action, out ActionOutcome outcome), nameof(Apply));
        return outcome;
    }

    // ---- Settings ----

    /// <summary>The reader's current typography and theme.</summary>
    public ReadingSettings Settings
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_settings(Live(), out NativeSettings s), nameof(Settings));
            return ReadingSettings.From(s);
        }
    }

    /// <summary>
    /// Change the settings, for this book or as the default.
    /// </summary>
    /// <remarks>
    /// This preserves the chosen font family rather than clearing it, so
    /// changing the size does not silently discard the typeface.
    /// </remarks>
    public void SetSettings(ReadingSettings settings, SettingsScope scope) =>
        ChapbookException.Check(
            Interop.cb_session_set_settings(Live(), settings.ToNative(), scope),
            nameof(SetSettings));

    /// <summary>
    /// The families this session can match — what a font picker offers.
    /// </summary>
    /// <remarks>
    /// The list grows as chapters load, because a book's own
    /// <c>@font-face</c> families join it when their unit lays out.
    /// </remarks>
    public IReadOnlyList<string> FontFamilies
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_font_family_count(Live(), out nuint count),
                nameof(FontFamilies));
            var families = new List<string>((int)count);
            for (nuint i = 0; i < count; i++)
            {
                nuint index = i;
                families.Add(
                    Strings.Read((byte[]? b, nuint c, out nuint n) =>
                        Interop.cb_session_font_family_at(Live(), index, b, c, out n),
                        nameof(FontFamilies)) ?? string.Empty);
            }
            return families;
        }
    }

    /// <summary>The reader's chosen family, or <c>null</c> for the publisher's.</summary>
    /// <remarks>
    /// The ABI answers with an empty string rather than an absence, and
    /// says the two mean the same thing on purpose. This folds them into
    /// <c>null</c> because that is .NET's own word for unset — it invents
    /// no distinction the ABI does not make, and it keeps a picker's
    /// "publisher's font" branch to one test.
    /// </remarks>
    public string? FontFamily
    {
        get
        {
            string? family = Strings.Read((byte[]? b, nuint c, out nuint n) =>
                Interop.cb_session_font_family(Live(), b, c, out n), nameof(FontFamily));
            return string.IsNullOrEmpty(family) ? null : family;
        }
    }

    /// <summary>
    /// Choose a typeface, or pass <c>null</c> to return the book to the
    /// publisher's.
    /// </summary>
    /// <remarks>
    /// The choice <b>beats</b> the publisher's <c>font-family</c>, unlike
    /// size and line height which lose to a publisher that specifies.
    /// That is deliberate: nearly every real EPUB sets
    /// <c>body { font-family }</c>, so a polite rule would do nothing on
    /// nearly every book. Monospace elements keep their font, because a
    /// code listing reflowed into a serif is a bug people report rather
    /// than a preference they expressed.
    /// </remarks>
    public void SetFontFamily(string? family, SettingsScope scope) =>
        ChapbookException.Check(
            Interop.cb_session_set_font_family(Live(), family, scope), nameof(SetFontFamily));

    /// <summary>How many faces the font database loaded.</summary>
    public int FontFaceCount
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_font_face_count(Live(), out nuint count),
                nameof(FontFaceCount));
            return (int)count;
        }
    }

    /// <summary>
    /// Any CSS generic that resolved to a family nothing carries, or
    /// <c>null</c> when all five landed.
    /// </summary>
    /// <remarks>
    /// Worth printing once at startup on a platform you have not run on:
    /// none of these failures announce themselves, and the symptom is a
    /// page of tofu rather than an error.
    /// </remarks>
    public string? UnresolvedFontGenerics =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_session_font_unresolved(Live(), b, c, out n),
            nameof(UnresolvedFontGenerics));

    // ---- Lifecycle ----

    /// <summary>
    /// Persist the position, close the database, drop the caches.
    /// </summary>
    /// <remarks>
    /// <para>
    /// Call it when the platform says the app is about to be stopped or
    /// the machine is shutting down, because that is the only guaranteed
    /// callback and it is on a clock. The session keeps working
    /// afterwards; the library reopens on the next access.
    /// </para>
    /// <para>
    /// <b>It is also the only way to leave a bookmark.</b> The Rust API
    /// has a <c>save_position</c> that persists and nothing else; the C
    /// ABI does not carry one, so across this boundary persisting a place
    /// means suspending. Disposing a session does <b>not</b> save — a book
    /// closed without this call reopens where it was last suspended, which
    /// on a first read is the beginning. Call it before you tear a session
    /// down, and after any jump you would be sorry to lose.
    /// </para>
    /// </remarks>
    public void Suspend() =>
        ChapbookException.Check(Interop.cb_session_suspend(Live()), nameof(Suspend));

    /// <summary>Give back everything but the page on screen.</summary>
    public void ReleaseCaches() =>
        ChapbookException.Check(Interop.cb_session_release_caches(Live()), nameof(ReleaseCaches));

    /// <summary>How many bytes the caches hold now.</summary>
    public long CacheBytes
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_cache_bytes(Live(), out nuint bytes), nameof(CacheBytes));
            return (long)bytes;
        }
    }

    /// <summary>The cap those caches are held to.</summary>
    public long CacheBudget
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_cache_budget(Live(), out nuint bytes), nameof(CacheBudget));
            return (long)bytes;
        }
    }

    // ---- Background loads ----

    /// <summary>
    /// Let the loader thread wake the host when a page lands.
    /// </summary>
    /// <remarks>
    /// <para>
    /// Comic pages and PDF rasterizations decode on the session's own
    /// worker. The callback fires <b>on that thread</b>, so it must be
    /// thread-safe and must not touch the session: post to your UI thread
    /// and do the work there. A host with nowhere to post can poll
    /// <see cref="HasPendingLoads"/> instead; a host that does neither
    /// shows placeholders forever.
    /// </para>
    /// <para>
    /// Passing <c>null</c> removes the waker.
    /// </para>
    /// </remarks>
    public unsafe void SetWaker(Action? wake)
    {
        nint handle = Live();
        if (_waker.IsAllocated)
        {
            _waker.Free();
        }
        if (wake is null)
        {
            ChapbookException.Check(Interop.cb_session_set_waker(handle, 0, 0), nameof(SetWaker));
            return;
        }
        // The delegate is pinned by the handle rather than by a field: the
        // native side keeps the pointer, and a collected target is a call
        // into freed memory on the first page that lands.
        _waker = GCHandle.Alloc(wake);
        ChapbookException.Check(
            Interop.cb_session_set_waker(
                handle,
                (nint)(delegate* unmanaged<nint, void>)&OnWake,
                GCHandle.ToIntPtr(_waker)),
            nameof(SetWaker));
    }

    [UnmanagedCallersOnly]
    private static void OnWake(nint user)
    {
        // Nothing may unwind back into Rust here; a host's bug becomes a
        // missed wakeup rather than a torn process.
        try
        {
            if (GCHandle.FromIntPtr(user).Target is Action wake)
            {
                wake();
            }
        }
        catch
        {
            // Deliberately swallowed. See above.
        }
    }

    /// <summary>
    /// Drain finished loads into the caches, and say whether <b>the page on
    /// screen</b> changed.
    /// </summary>
    /// <remarks>
    /// A prefetched unit landing does not count: nothing the reader can see
    /// moved, and treating it as a change costs a full repaint — on e-ink,
    /// a full panel flash.
    /// </remarks>
    public bool PollLoaded()
    {
        ChapbookException.Check(
            Interop.cb_session_poll_loaded(Live(), out byte changed), nameof(PollLoaded));
        return changed != 0;
    }

    /// <summary>
    /// Take the next event, oldest first, or <c>null</c> when there is
    /// none — which is the ordinary answer and not a failure.
    /// </summary>
    /// <remarks>
    /// <see cref="PollLoaded"/> answers "should I repaint"; this answers
    /// "is there anything to tell the reader", and a host needs both. It
    /// is a queue to drain rather than a callback to install, because the
    /// session mutates through itself and a handler fired from inside one
    /// could not call back into it.
    /// </remarks>
    public SessionEvent? NextEvent()
    {
        Status status = Interop.cb_session_next_event(Live(), out NativeSessionEvent evt);
        if (status == Status.Unavailable)
        {
            return null;
        }
        ChapbookException.Check(status, nameof(NextEvent));
        // The message is borrowed from the session and dies on the next
        // call, so it is copied here and nowhere later. Marshalling it
        // straight into a managed string is the copy.
        string? message = evt.Message == 0
            ? null
            : System.Runtime.InteropServices.Marshal.PtrToStringUTF8(evt.Message);
        return new SessionEvent(evt.Kind, (int)evt.Spine, (int)evt.Page, message);
    }

    /// <summary>
    /// Every event waiting, oldest first. The ordinary way to use
    /// <see cref="NextEvent"/>.
    /// </summary>
    /// <remarks>
    /// Drain after a wake or an action. The engine coalesces on its side —
    /// ten page turns between drains report one position change, not ten —
    /// so a host cannot miss a move by draining rarely.
    /// </remarks>
    public IReadOnlyList<SessionEvent> DrainEvents()
    {
        var events = new List<SessionEvent>();
        while (NextEvent() is { } evt)
        {
            events.Add(evt);
        }
        return events;
    }

    /// <summary>
    /// Whether any load is still in flight — what a placeholder page is
    /// telling the reader about.
    /// </summary>
    /// <remarks>
    /// It is also how a host knows when a freshly opened session has
    /// finished restoring the reader's place. A restored position resolves
    /// on the loader thread, so <see cref="Position"/> read immediately
    /// after <see cref="SetMetrics"/> can still be the beginning of the
    /// book: poll <see cref="PollLoaded"/> until this goes false, then
    /// read it.
    /// </remarks>
    public bool HasPendingLoads
    {
        get
        {
            ChapbookException.Check(
                Interop.cb_session_has_pending_loads(Live(), out byte pending),
                nameof(HasPendingLoads));
            return pending != 0;
        }
    }

    public void Dispose()
    {
        if (_handle != 0)
        {
            Interop.cb_session_close(_handle);
            _handle = 0;
        }
        if (_waker.IsAllocated)
        {
            _waker.Free();
        }
    }
}
