using System.Runtime.InteropServices;

namespace Chapbook;

// The plain-data types the ABI passes by value, and the enumerations it
// passes as codes. Layout here is a promise about `chapbook.h`, so every
// struct is `[StructLayout(LayoutKind.Sequential)]` and every field is in
// the header's order — a reordering compiles cleanly and misreads every
// value, which is the one bug this file can have.
//
// `size_t` is `nuint`, and the ABI's one-byte `bool` is a `byte` here
// rather than a `bool`. That is not pedantry: .NET's default marshalling
// for `bool` in a struct is a four-byte Win32 `BOOL`, which would misread
// every field after it — and with runtime marshalling disabled for this
// assembly (see `AssemblyInfo.cs`) a non-blittable field is a compile
// error rather than a silent one, which is the trade this file wants.

[StructLayout(LayoutKind.Sequential)]
internal struct NativeMetrics
{
    public float Width;
    public float Height;
    public float MarginTop;
    public float MarginRight;
    public float MarginBottom;
    public float MarginLeft;
    public float DpiScale;
    public Rotation Rotation;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativePosition
{
    public uint Spine;
    public uint Page;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeSettings
{
    public float BaseFontPx;
    public float LineHeight;
    public byte Justify;
    public byte PublisherStyles;
    public Theme Theme;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeRect
{
    public float X;
    public float Y;
    public float W;
    public float H;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeTextRun
{
    public NativeRect Rect;
    public uint LocatorStart;
    public uint LocatorEnd;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeWordSpan
{
    public uint TextStart;
    public uint TextEnd;
    public uint LocatorStart;
    public uint LocatorEnd;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeSessionEvent
{
    public SessionEventKind Kind;
    public nuint Spine;
    public nuint Page;
    /// <summary>
    /// Borrowed from the session and valid only until the next event is
    /// taken or the session closes. It is copied into a managed string the
    /// moment it arrives, which is the only correct thing to do with it.
    /// </summary>
    public nint Message;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeBookQuery
{
    public nint Search;
    public nint Series;
    public long Collection;
    public ReadingState State;
    public ShelfSort Sort;
    public nuint Limit;
    public nuint Offset;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeBook
{
    public long Id;
    public long AddedAt;
    public long LastRead;
    public long FinishedAt;
    public double Progress;
    public double SeriesIndex;
    public ReadingState State;
    public nuint AuthorCount;
    public nuint CollectionCount;
    public byte HasProgress;
    public byte HasSeriesIndex;
    public byte HasCover;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeCollection
{
    public long Id;
    public long AddedAt;
    public nuint Books;
}

/// <summary>
/// The ABI's result code. Zero is success, negative is failure, and the
/// numbers are permanent — the human-readable half is
/// <see cref="ChapbookException.Message"/> and is explicitly free to
/// change, so match on this and never on that.
/// </summary>
public enum Status
{
    Ok = 0,
    Panic = -1,
    NullArgument = -2,
    InvalidUtf8 = -3,
    BufferTooSmall = -4,
    InvalidArgument = -5,
    Unavailable = -6,
    BookOpen = -10,
    BookMalformed = -11,
    ResourceNotFound = -12,
    FixedLayoutUnsupported = -13,
    FormatNotBuilt = -14,
    SpineOutOfRange = -15,
    Parse = -16,
    Style = -17,
    Layout = -18,
    Font = -19,
    Cfi = -20,
    Network = -21,
    Opds = -22,
    LibraryError = -23,
    Credential = -24,
    Panel = -25,
    Io = -26,

    /// <summary>
    /// The catalogue wants credentials and said so with an authentication
    /// document. A response, not a failure: read it through
    /// <see cref="Catalog.AuthTitle"/> and <see cref="Catalog.AuthOffersBasic"/>,
    /// put up a login, <see cref="Catalog.SignIn"/>, and fetch again.
    /// </summary>
    AuthRequired = -27,
}

/// <summary>Which format to read bytes as. <see cref="Guess"/> sniffs.</summary>
public enum BookFormat : uint
{
    Guess = 0,
    Epub = 1,
    Cbz = 2,
    Pdf = 3,
}

/// <summary>What kind of thing the open publication is.</summary>
public enum BookKind : uint
{
    Epub = 0,
    Comic = 1,
    Pdf = 2,
}

/// <summary>
/// Which physical edge reading starts from. The book declares it — EPUB's
/// <c>page-progression-direction</c> — and a host needs it so its own
/// gestures agree with the tap zones.
/// </summary>
public enum ReadingDirection : uint
{
    LeftToRight = 0,
    RightToLeft = 1,
}

/// <summary>The page's colour scheme.</summary>
public enum Theme : uint
{
    Light = 0,
    Sepia = 1,
    Dark = 2,
}

/// <summary>
/// A quarter turn applied on the way to the panel. Not a layout input:
/// turning the panel does not repaginate the book.
/// </summary>
public enum Rotation : uint
{
    None = 0,
    Quarter = 1,
    Half = 2,
    ThreeQuarter = 3,
}

/// <summary>Whether a settings change is this book's or the default.</summary>
public enum SettingsScope : uint
{
    Global = 0,
    ThisBook = 1,
}

/// <summary>
/// A reader intent. A host produces one — from a tap zone, a key lookup,
/// or its own UI — and applies it; it never has to interpret one.
/// </summary>
/// <remarks>
/// The set is open and values are only appended, so a host may persist
/// them. <see cref="None"/> is not an action: it is the "nothing" answer,
/// and applying it is an error.
/// </remarks>
public enum ReaderAction : uint
{
    None = 0,
    NextPage = 1,
    PrevPage = 2,
    NextUnit = 3,
    PrevUnit = 4,
    Back = 5,
    FontUp = 6,
    FontDown = 7,
    CycleTheme = 8,
    ToggleMenu = 9,
}

/// <summary>
/// A key in the engine's vocabulary, not a platform keycode. A host
/// translates its own codes into this; the opinions live in
/// <see cref="Engine.DefaultAction(Key)"/>.
/// </summary>
public enum Key : uint
{
    ArrowLeft = 1,
    ArrowRight = 2,
    ArrowUp = 3,
    ArrowDown = 4,
    PageUp = 5,
    PageDown = 6,
    Space = 7,
    Backspace = 8,
    TurnPrev = 9,
    TurnNext = 10,
    VolumeUp = 11,
    VolumeDown = 12,
}

/// <summary>
/// What the engine did with an action — two answers, because a host needs
/// both and can derive neither from the other.
/// </summary>
public enum ActionOutcome : uint
{
    /// <summary>Applied and something moved: consume the event and repaint.</summary>
    Changed = 0,

    /// <summary>
    /// Applied and nothing moved — the last page, or the font at its stop.
    /// Consume the event anyway; the reader does take this key.
    /// </summary>
    Unchanged = 1,

    /// <summary>
    /// Not the engine's. Let the event through to the platform — which is
    /// also what <see cref="ReaderAction.Back"/> answers at the bottom of
    /// the back trail, so a host forwards the gesture unconditionally
    /// rather than shadowing the history to know when to stop.
    /// </summary>
    Unhandled = 2,
}

/// <summary>What kind of thing a <see cref="SessionEvent"/> is reporting.</summary>
/// <remarks>
/// The underlying type is <c>int</c> and not <c>uint</c>, unlike every
/// other enumeration here. That is not a slip: this one is <c>repr(C)</c>
/// on the Rust side where the others are <c>repr(u32)</c>, so its size is
/// a C <c>int</c>. Both are four bytes on every target chapbook builds
/// for, and writing down which is which is cheaper than finding out.
/// </remarks>
public enum SessionEventKind
{
    /// <summary>
    /// A background unit finished decoding, prefetches included.
    /// <see cref="Session.PollLoaded"/> deliberately answers false for
    /// those, and a host watching load progress wants both answers.
    /// </summary>
    UnitLoaded = 0,

    /// <summary>
    /// A unit failed and will not be retried. Without this a comic page
    /// that failed to download stays a placeholder forever with nothing
    /// able to say why.
    /// </summary>
    UnitFailed = 1,

    /// <summary>
    /// The reader is somewhere else — including moves the host did not
    /// make: a restored position resolving after open, a load landing that
    /// settles the page.
    /// </summary>
    PositionChanged = 2,

    /// <summary>
    /// The last page of the last unit, on the transition rather than on
    /// every drain, re-arming if the reader leaves and comes back. Whether
    /// it means "mark as read" is the host's policy.
    /// </summary>
    BookFinished = 3,
}

/// <summary>How much of a book has been read.</summary>
public enum ReadingState : uint
{
    Any = 0,
    Unread = 1,
    Reading = 2,
    Finished = 3,
}

/// <summary>The order a shelf comes back in.</summary>
public enum ShelfSort : uint
{
    Added = 0,
    Read = 1,
    Title = 2,
    Author = 3,
    Series = 4,
}

/// <summary>How much the engine says, and about what.</summary>
public enum LogLevel
{
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

/// <summary>
/// What the loaded library was built with. A header cannot say which
/// artifact a host actually loaded, which is why this is a runtime
/// question.
/// </summary>
[Flags]
public enum Capabilities : uint
{
    None = 0,
    Library = 1,
    Cbz = 2,
    Pdf = 4,
    Opds = 8,
    BundledHttp = 16,
    Svg = 32,
    MathMl = 64,
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeAnnotation
{
    public long Id;
    public AnnotationKind Kind;
    public nuint Spine;
    public double Progression;
    public byte HasText;
    public byte HasColor;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeTocEntry
{
    public nuint Depth;
    public nuint Spine;
    public byte HasSpine;
    public byte HasFragment;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeSearchHit
{
    public nuint Spine;
    public uint Start;
    public uint End;
    public uint MatchStart;
    public uint MatchEnd;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeEntry
{
    public EntryKind Kind;
    public nuint AuthorCount;
    public byte CanDownload;
    public byte IsOpenAccess;
    public byte HasThumbnail;
    public byte HasCover;
    public byte HasSummary;
    public byte HasSeries;
    public double SeriesPosition;
    public byte HasSeriesPosition;
    public byte SyncsPosition;
    public byte SyncsAnnotations;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeFacet
{
    public nuint Group;
    public byte Active;
    public ulong Count;
    public byte HasCount;
}

/// <summary>What kind of mark an <see cref="Annotation"/> is.</summary>
/// <remarks>
/// <c>int</c>-backed like <see cref="SessionEventKind"/>, because the
/// Rust side is <c>repr(C)</c> rather than <c>repr(u32)</c>. Same for
/// every enumeration below this line.
/// </remarks>
public enum AnnotationKind
{
    /// <summary>A point remembered, nothing painted.</summary>
    Bookmark = 0,

    /// <summary>A range painted on the page.</summary>
    Highlight = 1,

    /// <summary>A range with words attached.</summary>
    Note = 2,
}

/// <summary>What a catalogue row is, which decides what tapping it does.</summary>
public enum EntryKind
{
    /// <summary>
    /// A place to go: a shelf, a section, another feed. Tapping it fetches
    /// <see cref="CatalogEntry.Href"/>.
    /// </summary>
    Navigation = 0,

    /// <summary>A book. Tapping it downloads.</summary>
    Publication = 1,
}

/// <summary>Which of an entry's strings to read.</summary>
internal enum EntryField
{
    Title = 0,
    Summary = 1,
    Publisher = 2,
    Language = 3,
    Series = 4,
    ThumbnailUrl = 5,
    CoverUrl = 6,
    Href = 7,
}

/// <summary>Which of a facet's strings to read.</summary>
internal enum FacetField
{
    Label = 0,
    Group = 1,
    Href = 2,
}

/// <summary>Which way through a paged feed.</summary>
internal enum CatalogPage
{
    Next = 0,
    Previous = 1,
}
