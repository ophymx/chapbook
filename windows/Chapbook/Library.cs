using System.Runtime.InteropServices;

namespace Chapbook;

/// <summary>What to list, and in what order.</summary>
/// <remarks>
/// Default-constructed is the whole shelf, newest first. Every field
/// narrows.
/// </remarks>
public sealed record ShelfQuery
{
    /// <summary>
    /// Free text over title, authors and series, or <c>null</c> for no
    /// text filter.
    /// </summary>
    /// <remarks>
    /// Whole words matched by prefix, folded for case <i>and</i> accents,
    /// so "bronte" finds Brontë. Text holding nothing searchable —
    /// punctuation alone — matches no book rather than every book, which
    /// is what a search field wants while someone is still typing.
    /// </remarks>
    public string? Search { get; init; }

    /// <summary>
    /// Only this series, matched exactly but case-folded, or <c>null</c>
    /// for any. The value comes from a row rather than from typing.
    /// </summary>
    public string? Series { get; init; }

    /// <summary>Only books in this collection; <c>null</c> for any.</summary>
    public long? Collection { get; init; }

    /// <summary>How much of a book has been read.</summary>
    public ReadingState State { get; init; } = ReadingState.Any;

    /// <summary>The order rows come back in.</summary>
    public ShelfSort Sort { get; init; } = ShelfSort.Added;

    /// <summary>How many rows to return; <c>null</c> for all of them.</summary>
    public int? Limit { get; init; }

    /// <summary>How many to skip — the other half of paging a long shelf.</summary>
    public int Offset { get; init; }
}

/// <summary>One row of the shelf.</summary>
/// <param name="Id">The library row, which is what every other call names a book by.</param>
/// <param name="Progress">
/// How far through, from 0 to 1, or <c>null</c> for a book never opened.
/// </param>
/// <param name="SeriesIndex">Where in its series, or <c>null</c>.</param>
/// <param name="CoverPath">A cover on disk, or <c>null</c> when there is none.</param>
public sealed record ShelfBook(
    long Id,
    string Title,
    IReadOnlyList<string> Authors,
    string? Series,
    double? SeriesIndex,
    string? Language,
    string? Identifier,
    string Fingerprint,
    string? FilePath,
    string? CoverPath,
    ReadingState State,
    double? Progress,
    DateTimeOffset AddedAt,
    DateTimeOffset? LastRead,
    DateTimeOffset? FinishedAt,
    IReadOnlyList<CollectionRef> Collections);

/// <summary>A collection a book belongs to, as a row carries it.</summary>
public readonly record struct CollectionRef(long Id, string Name);

/// <summary>A collection, with how many books are in it.</summary>
public sealed record Collection(long Id, string Name, int Books, DateTimeOffset AddedAt);

/// <summary>
/// The shelf: what the reader owns, where they are in it, and how it is
/// grouped.
/// </summary>
/// <remarks>
/// <para>
/// It sits <i>beside</i> a session rather than replacing one. A book
/// reaches the library by being <b>opened</b>, so an app's "add to
/// library" is a read, and <see cref="Session.BookId"/> says which row
/// that became. There is deliberately no import call.
/// </para>
/// <para>
/// Unlike a session, a library may be held <i>while</i> one is open: the
/// database is WAL, and two connections is the ordinary way to draw a
/// shelf while a book is being read. Like a session, one instance is not
/// for two threads at once.
/// </para>
/// <para>
/// A query's rows are copied out rather than left behind a cursor,
/// because a search field issues a query per keystroke and the list being
/// drawn must not move underneath the draw.
/// </para>
/// </remarks>
public sealed class Library : IDisposable
{
    private nint _handle;

    /// <summary>Open the library in a directory, creating it if needed.</summary>
    public Library(string directory)
    {
        ChapbookException.Check(Interop.cb_library_open(directory, out _handle), nameof(Library));
    }

    /// <summary>
    /// Open the library where this platform keeps one — on Windows
    /// <c>%APPDATA%\chapbook</c>, else <c>%LOCALAPPDATA%</c>, and
    /// <c>CHAPBOOK_LIBRARY_DIR</c> ahead of both.
    /// </summary>
    public static Library Default() => new(DefaultDirectory());

    /// <summary>Where <see cref="Default"/> would look.</summary>
    public static string DefaultDirectory() =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_library_default_dir(b, c, out n), nameof(DefaultDirectory))
        ?? throw ChapbookException.From(Status.Unavailable, nameof(DefaultDirectory));

    private nint Live() =>
        _handle != 0 ? _handle : throw new ObjectDisposedException(nameof(Library));

    /// <summary>The rows a query names.</summary>
    /// <remarks>
    /// An empty shelf is an empty list, not a failure.
    /// </remarks>
    public IReadOnlyList<ShelfBook> Books(ShelfQuery? query = null)
    {
        query ??= new ShelfQuery();
        nint search = Utf8(query.Search);
        nint series = Utf8(query.Series);
        try
        {
            var native = new NativeBookQuery
            {
                Search = search,
                Series = series,
                Collection = query.Collection ?? 0,
                State = query.State,
                Sort = query.Sort,
                Limit = (nuint)Math.Max(0, query.Limit ?? 0),
                Offset = (nuint)Math.Max(0, query.Offset),
            };
            ChapbookException.Check(
                Interop.cb_library_query(Live(), in native, out nint shelf), nameof(Books));
            try
            {
                return ReadShelf(shelf);
            }
            finally
            {
                Interop.cb_shelf_free(shelf);
            }
        }
        finally
        {
            // The engine copied whatever it needed during the call; these
            // are ours to release either way.
            if (search != 0) { Marshal.FreeCoTaskMem(search); }
            if (series != 0) { Marshal.FreeCoTaskMem(series); }
        }
    }

    private static IReadOnlyList<ShelfBook> ReadShelf(nint shelf)
    {
        ChapbookException.Check(Interop.cb_shelf_len(shelf, out nuint len), nameof(Books));
        var books = new List<ShelfBook>((int)len);
        for (nuint i = 0; i < len; i++)
        {
            nuint row = i;
            ChapbookException.Check(
                Interop.cb_shelf_book(shelf, row, out NativeBook b), nameof(Books));

            var authors = new List<string>((int)b.AuthorCount);
            for (nuint a = 0; a < b.AuthorCount; a++)
            {
                nuint slot = a;
                authors.Add(Read(
                    (byte[]? buf, nuint cap, out nuint need) =>
                        Interop.cb_shelf_author(shelf, row, slot, buf, cap, out need))
                    ?? string.Empty);
            }

            var collections = new List<CollectionRef>((int)b.CollectionCount);
            for (nuint c = 0; c < b.CollectionCount; c++)
            {
                nuint slot = c;
                ChapbookException.Check(
                    Interop.cb_shelf_collection_id(shelf, row, slot, out long id), nameof(Books));
                collections.Add(new CollectionRef(id, Read(
                    (byte[]? buf, nuint cap, out nuint need) =>
                        Interop.cb_shelf_collection_name(shelf, row, slot, buf, cap, out need))
                    ?? string.Empty));
            }

            books.Add(new ShelfBook(
                Id: b.Id,
                Title: Read((byte[]? buf, nuint cap, out nuint need) =>
                    Interop.cb_shelf_title(shelf, row, buf, cap, out need)) ?? string.Empty,
                Authors: authors,
                Series: Read((byte[]? buf, nuint cap, out nuint need) =>
                    Interop.cb_shelf_series(shelf, row, buf, cap, out need)),
                SeriesIndex: b.HasSeriesIndex != 0 ? b.SeriesIndex : null,
                Language: Read((byte[]? buf, nuint cap, out nuint need) =>
                    Interop.cb_shelf_language(shelf, row, buf, cap, out need)),
                Identifier: Read((byte[]? buf, nuint cap, out nuint need) =>
                    Interop.cb_shelf_identifier(shelf, row, buf, cap, out need)),
                Fingerprint: Read((byte[]? buf, nuint cap, out nuint need) =>
                    Interop.cb_shelf_fingerprint(shelf, row, buf, cap, out need)) ?? string.Empty,
                FilePath: Read((byte[]? buf, nuint cap, out nuint need) =>
                    Interop.cb_shelf_file_path(shelf, row, buf, cap, out need)),
                CoverPath: b.HasCover != 0
                    ? Read((byte[]? buf, nuint cap, out nuint need) =>
                        Interop.cb_shelf_cover_path(shelf, row, buf, cap, out need))
                    : null,
                State: b.State,
                Progress: b.HasProgress != 0 ? b.Progress : null,
                AddedAt: DateTimeOffset.FromUnixTimeSeconds(b.AddedAt),
                LastRead: Moment(b.LastRead),
                FinishedAt: Moment(b.FinishedAt),
                Collections: collections));
        }
        return books;
    }

    /// <summary>Every collection, with its size.</summary>
    public IReadOnlyList<Collection> Collections()
    {
        Status sizing = Interop.cb_library_collections(Live(), null, 0, out nuint needed);
        if (needed == 0)
        {
            return [];
        }
        if (sizing != Status.BufferTooSmall)
        {
            ChapbookException.Check(sizing, nameof(Collections));
        }
        var buffer = new NativeCollection[needed];
        ChapbookException.Check(
            Interop.cb_library_collections(Live(), buffer, needed, out _), nameof(Collections));

        var collections = new List<Collection>(buffer.Length);
        foreach (NativeCollection c in buffer)
        {
            long id = c.Id;
            collections.Add(new Collection(
                id,
                Read((byte[]? buf, nuint cap, out nuint need) =>
                    Interop.cb_library_collection_name(Live(), id, buf, cap, out need))
                    ?? string.Empty,
                (int)c.Books,
                DateTimeOffset.FromUnixTimeSeconds(c.AddedAt)));
        }
        return collections;
    }

    /// <summary>Make a collection and return its id.</summary>
    public long CreateCollection(string name)
    {
        ChapbookException.Check(
            Interop.cb_library_create_collection(Live(), name, out long id),
            nameof(CreateCollection));
        return id;
    }

    /// <summary>Rename one.</summary>
    public void RenameCollection(long collection, string name) =>
        ChapbookException.Check(
            Interop.cb_library_rename_collection(Live(), collection, name),
            nameof(RenameCollection));

    /// <summary>Remove one. The books in it stay on the shelf.</summary>
    public void DeleteCollection(long collection) =>
        ChapbookException.Check(
            Interop.cb_library_delete_collection(Live(), collection), nameof(DeleteCollection));

    /// <summary>Put a book in a collection.</summary>
    public void AddToCollection(long book, long collection) =>
        ChapbookException.Check(
            Interop.cb_library_add_to_collection(Live(), book, collection),
            nameof(AddToCollection));

    /// <summary>Take it out again.</summary>
    public void RemoveFromCollection(long book, long collection) =>
        ChapbookException.Check(
            Interop.cb_library_remove_from_collection(Live(), book, collection),
            nameof(RemoveFromCollection));

    /// <summary>Take a book off the shelf.</summary>
    public void DeleteBook(long book) =>
        ChapbookException.Check(Interop.cb_library_delete_book(Live(), book), nameof(DeleteBook));

    /// <summary>
    /// Put a file on the shelf, answering with the row it became.
    /// </summary>
    /// <remarks>
    /// <para>
    /// <b>Where a download the app ran itself comes back.</b> Take a
    /// <see cref="Catalog.DownloadRequest"/>, fetch it with
    /// <c>BackgroundTransferSession</c> or an <c>HttpClient</c> of your
    /// own, and hand the finished file here. The format is read from the
    /// bytes, so whatever the transfer named the file is fine.
    /// </para>
    /// <para>
    /// The file is not consumed: the library copies what it imports and
    /// never deletes the source, which is yours — unlike
    /// <see cref="Catalog.Download(CatalogEntry, string)"/>, which removes
    /// the staging file it made itself. Importing the same bytes twice
    /// answers with the same row rather than shelving a duplicate, so a
    /// retried or twice-delivered transfer needs no coordination with
    /// this call.
    /// </para>
    /// <para>
    /// Sync services are not in the file. They live in the catalog entry,
    /// so pass the progression URL and annotation container captured in
    /// the <see cref="Catalog.DownloadRequest"/> — <i>before</i> the
    /// transfer, while the feed was open — to
    /// <see cref="SetSyncTargets"/> once this returns a row.
    /// </para>
    /// <para><b>Blocking</b>: it copies a whole book.</para>
    /// </remarks>
    public long ImportFile(string path)
    {
        ChapbookException.Check(
            Interop.cb_library_import_file(Live(), path, out long book), nameof(ImportFile));
        return book;
    }

    /// <summary>Mark a book read, or unread again.</summary>
    public void SetFinished(long book, bool finished) =>
        ChapbookException.Check(
            Interop.cb_library_set_finished(Live(), book, finished ? (byte)1 : (byte)0),
            nameof(SetFinished));

    // ---- Sync targets ----

    /// <summary>
    /// Record where a book syncs: its OPDS Progression endpoint and its
    /// Web Annotation container, either or both.
    /// </summary>
    /// <remarks>
    /// <para>
    /// These are the two service links off the catalogue entry a book was
    /// downloaded from, which is the only place a book learns them — in
    /// both protocols the URL <i>is</i> the publication's identity, while
    /// the library keys books by an edition fingerprint, and this call is
    /// what closes that gap. A sideloaded book has no entry and so no
    /// service.
    /// </para>
    /// <para>
    /// Calling again replaces both values; two nulls make the book local
    /// again without touching what it still owes. <b>Both URLs are opaque
    /// and may embed a per-user key</b> — never log them, and key any
    /// credential by origin rather than by URL.
    /// </para>
    /// </remarks>
    public void SetSyncTargets(long book, string? progressionUrl, string? annotationContainer) =>
        ChapbookException.Check(
            Interop.cb_library_set_sync_targets(
                Live(), book, progressionUrl, annotationContainer),
            nameof(SetSyncTargets));

    /// <summary>
    /// The progression service this book syncs its position to, or
    /// <c>null</c> — the ordinary state of a sideloaded book.
    /// </summary>
    public string? SyncProgressionUrl(long book) =>
        Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_library_sync_progression_url(Live(), book, b, c, out n));

    /// <summary>The Web Annotation container this book syncs marks with, or <c>null</c>.</summary>
    public string? SyncAnnotationContainer(long book) =>
        Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_library_sync_annotation_container(Live(), book, b, c, out n));

    private static string? Read(StringOut call) => Strings.Read(call, nameof(Library));

    private static DateTimeOffset? Moment(long seconds) =>
        seconds == 0 ? null : DateTimeOffset.FromUnixTimeSeconds(seconds);

    /// <summary>
    /// A NUL-terminated UTF-8 copy for the engine to read during one call,
    /// or a null pointer for "no filter".
    /// </summary>
    private static nint Utf8(string? value) =>
        value is null ? 0 : Marshal.StringToCoTaskMemUTF8(value);

    public void Dispose()
    {
        if (_handle != 0)
        {
            Interop.cb_library_close(_handle);
            _handle = 0;
        }
    }
}
