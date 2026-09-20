namespace Chapbook;

/// <summary>One row of a catalogue listing.</summary>
/// <param name="Index">
/// The row's index in the held feed, which is what
/// <see cref="Catalog.Download(CatalogEntry, string)"/> takes. Valid until
/// the next <see cref="Catalog.Fetch"/> or <see cref="Catalog.Search"/>.
/// </param>
/// <param name="Kind">What tapping the row does.</param>
/// <param name="Summary">
/// Plain text. HTML descriptions are deliberately not offered: a host
/// would have to sanitise what it did not parse.
/// </param>
/// <param name="SeriesPosition">
/// Where in its series, when the entry says. Only ever set alongside
/// <paramref name="Series"/>.
/// </param>
/// <param name="ThumbnailUrl">
/// Absolute. Fetch it with the platform's own image loader, sending the
/// same <c>Authorization</c> if the catalogue wants one. Images cross as
/// URLs, never bytes: a cover grid is what an image cache is for.
/// </param>
/// <param name="CoverUrl">A full cover, likewise, for a detail screen.</param>
/// <param name="Href">
/// Where tapping goes: the feed a navigation row points at, or a
/// publication's acquisition. A commercial entry whose only link is a
/// purchase page answers with that page, which a host opens in a browser
/// rather than downloading.
/// </param>
/// <param name="CanDownload">
/// Whether <see cref="Catalog.Download(CatalogEntry, string)"/> can take
/// this row at all. A purchase-only entry answers <c>false</c>.
/// </param>
/// <param name="IsOpenAccess">
/// Freely downloadable, as against borrowed or bought. What tells a "Get"
/// button from a "Buy" one.
/// </param>
/// <param name="SyncsPosition">
/// Whether the entry advertises a position-sync service. A host does not
/// have to act on it — the download records it — but a catalogue that
/// syncs is worth saying so on the row.
/// </param>
/// <param name="SyncsAnnotations">Likewise, an annotation container.</param>
public sealed record CatalogEntry(
    int Index,
    EntryKind Kind,
    string Title,
    IReadOnlyList<string> Authors,
    string? Summary,
    string? Publisher,
    string? Language,
    string? Series,
    double? SeriesPosition,
    string? ThumbnailUrl,
    string? CoverUrl,
    string? Href,
    bool CanDownload,
    bool IsOpenAccess,
    bool SyncsPosition,
    bool SyncsAnnotations);

/// <summary>
/// One facet: a way to narrow the held feed, as the catalogue offers it.
/// </summary>
/// <param name="Label">The facet's own label — "English", "By title".</param>
/// <param name="GroupName">Its group's name — "Language", "Sort by".</param>
/// <param name="Group">
/// Which group it belongs to, as an index. Facets in a group are
/// alternatives; a host draws one control per group.
/// </param>
/// <param name="Href">The URL that applies it; hand it to <see cref="Catalog.Fetch"/>.</param>
/// <param name="Active">Whether this facet is the one currently in force.</param>
/// <param name="Count">How many entries it would show, when the catalogue says.</param>
public sealed record CatalogFacet(
    string Label, string GroupName, int Group, string Href, bool Active, long? Count);

/// <summary>
/// An OPDS catalogue, browsed one feed at a time.
/// </summary>
/// <remarks>
/// <para>
/// <b>Every call that touches the network blocks</b> — <see cref="Fetch"/>,
/// <see cref="Search"/> and above all
/// <see cref="Download(CatalogEntry, string)"/>. Run them off the UI
/// thread, on a <see cref="Task"/> whose cancellation is the host's own;
/// this binding deliberately invents no worker, because the platform's is
/// better than anything it could.
/// </para>
/// <para>
/// The transport is the host's, on the same terms as everywhere else — a
/// catalogue behind a proxy, a user CA or a corporate network works
/// because the host's <see cref="HttpClient"/> does. The parameterless
/// constructor uses the engine's bundled transport instead, where this
/// build has one, and throws <see cref="Status.FormatNotBuilt"/> where it
/// does not.
/// </para>
/// <para>
/// <b>A 401 is an answer.</b> A fetch the catalogue refuses for want of
/// credentials throws with <see cref="Status.AuthRequired"/> and keeps the
/// authentication document: <see cref="AuthTitle"/> and
/// <see cref="AuthOffersBasic"/> are what a login sheet draws,
/// <see cref="SignIn"/> is what it submits, and the fetch is then simply
/// tried again.
/// </para>
/// <para>
/// Like a session, one instance is not for two threads at once.
/// </para>
/// </remarks>
public sealed class Catalog : IDisposable
{
    private nint _handle;

    /// <summary>A catalogue fetched through the host's own networking.</summary>
    /// <remarks>
    /// The transport is owned by the engine from this call onward and is
    /// released — <see cref="HttpTransport.OnReleased"/> — when the
    /// catalogue is disposed, or on this constructor's own failure.
    /// </remarks>
    public Catalog(HttpTransport transport)
    {
        ArgumentNullException.ThrowIfNull(transport);
        nint user = TransportBridge.Pin(transport);
        unsafe
        {
            ChapbookException.Check(
                Interop.cb_catalog_open(
                    (nint)TransportBridge.Get, (nint)TransportBridge.Finalize, user,
                    out _handle),
                nameof(Catalog));
        }
    }

    /// <summary>
    /// A catalogue over the engine's bundled transport, where this build
    /// has one.
    /// </summary>
    public Catalog()
    {
        ChapbookException.Check(
            Interop.cb_catalog_open(0, 0, 0, out _handle), nameof(Catalog));
    }

    private nint Live() =>
        _handle != 0 ? _handle : throw new ObjectDisposedException(nameof(Catalog));

    // ---- Credentials ----

    /// <summary>
    /// Send this <c>Authorization</c> header value with every request — a
    /// bearer token, or whatever the catalogue's own scheme wants.
    /// <c>null</c> clears it.
    /// </summary>
    /// <remarks>
    /// The value is opaque and never parsed. Key any store you keep it in
    /// by <i>origin</i>, not by the catalogue URL: a catalogue URL's path
    /// can itself be a secret.
    /// </remarks>
    public void SetAuthorization(string? value) =>
        ChapbookException.Check(
            Interop.cb_catalog_set_authorization(Live(), value), nameof(SetAuthorization));

    /// <summary>
    /// Sign in with a username and password — the HTTP Basic flow, which is
    /// what an OPDS authentication document offers when it offers anything.
    /// </summary>
    /// <remarks>
    /// The encoding is done in the engine on purpose, so no host has to
    /// guess whether the credential is UTF-8 first.
    /// </remarks>
    public void SignIn(string username, string password)
    {
        ArgumentNullException.ThrowIfNull(username);
        ArgumentNullException.ThrowIfNull(password);
        ChapbookException.Check(
            Interop.cb_catalog_set_basic_auth(Live(), username, password), nameof(SignIn));
    }

    // ---- Browsing ----

    /// <summary>
    /// Fetch what is at a URL and hold it — a catalogue root, a section a
    /// navigation row pointed at, a facet's narrowing, a page of a long
    /// feed. Whatever was held before is replaced.
    /// </summary>
    /// <remarks>
    /// <b>Blocking.</b> Throws with <see cref="Status.AuthRequired"/> when
    /// the catalogue wants credentials and said so properly; the
    /// authentication document is then held for <see cref="AuthTitle"/>
    /// and <see cref="AuthOffersBasic"/>.
    /// </remarks>
    public void Fetch(string url)
    {
        ArgumentNullException.ThrowIfNull(url);
        ChapbookException.Check(Interop.cb_catalog_fetch(Live(), url), nameof(Fetch));
    }

    /// <summary>
    /// Search the catalogue that is currently held. The results replace
    /// it, so browsing and searching are the same screen.
    /// </summary>
    /// <remarks>
    /// Throws with <see cref="Status.Unavailable"/> when this catalogue
    /// offers no search — <see cref="HasSearch"/> is worth asking before
    /// drawing a box. Blocking, like the fetch.
    /// </remarks>
    public void Search(string query)
    {
        ArgumentNullException.ThrowIfNull(query);
        ChapbookException.Check(Interop.cb_catalog_search(Live(), query), nameof(Search));
    }

    /// <summary>The held feed's title — what a browse screen puts at the top.</summary>
    public string? FeedTitle =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_catalog_feed_title(Live(), b, c, out n), nameof(FeedTitle));

    /// <summary>
    /// Whether this catalogue offers a search — what decides if a search
    /// box is drawn at all. <c>false</c> before anything is fetched.
    /// </summary>
    public bool HasSearch
    {
        get
        {
            Status status = Interop.cb_catalog_has_search(Live(), out byte has);
            if (status == Status.Unavailable)
            {
                return false;
            }
            ChapbookException.Check(status, nameof(HasSearch));
            return has != 0;
        }
    }

    /// <summary>Every row of the held feed.</summary>
    public IReadOnlyList<CatalogEntry> Entries()
    {
        ChapbookException.Check(
            Interop.cb_catalog_entry_count(Live(), out nuint count), nameof(Entries));
        var entries = new List<CatalogEntry>((int)count);
        for (nuint i = 0; i < count; i++)
        {
            nuint index = i;
            ChapbookException.Check(
                Interop.cb_catalog_entry(Live(), index, out NativeEntry row), nameof(Entries));

            var authors = new List<string>((int)row.AuthorCount);
            for (nuint a = 0; a < row.AuthorCount; a++)
            {
                nuint author = a;
                authors.Add(
                    Strings.Read((byte[]? b, nuint c, out nuint n) =>
                        Interop.cb_catalog_entry_author(Live(), index, author, b, c, out n),
                        nameof(Entries)) ?? string.Empty);
            }

            entries.Add(new CatalogEntry(
                (int)index,
                row.Kind,
                Text(index, EntryField.Title) ?? string.Empty,
                authors,
                Text(index, EntryField.Summary),
                Text(index, EntryField.Publisher),
                Text(index, EntryField.Language),
                Text(index, EntryField.Series),
                row.HasSeries != 0 && row.HasSeriesPosition != 0 ? row.SeriesPosition : null,
                Text(index, EntryField.ThumbnailUrl),
                Text(index, EntryField.CoverUrl),
                Text(index, EntryField.Href),
                row.CanDownload != 0,
                row.IsOpenAccess != 0,
                row.SyncsPosition != 0,
                row.SyncsAnnotations != 0));
        }
        return entries;
    }

    /// <summary>
    /// Everything needed to fetch one book yourself, for a transfer that
    /// has to outlive the window that started it.
    /// </summary>
    /// <remarks>
    /// <para>
    /// <see cref="Download(CatalogEntry, string)"/> runs the whole
    /// transfer inside one blocking call, which is right for a click the
    /// reader is watching and wrong for anything else — the process has
    /// to stay alive for it. Take one of these instead, hand it to
    /// <c>BackgroundTransferSession</c> or your own downloader, and call
    /// <see cref="Library.ImportFile"/> when the file lands.
    /// </para>
    /// <para>
    /// <b>Every field is advice except <c>Url</c>.</b> Rename the file,
    /// add headers, route it however the app routes things. The engine
    /// reads a book by its bytes, so the name it arrives under is free.
    /// </para>
    /// <para>
    /// <b>No credential travels in here, deliberately.</b> The app opened
    /// this catalogue, so it already knows which credential it takes — set
    /// <c>Authorization</c> when the transfer starts. That keeps the
    /// secret out of anything the transfer persists, and means a token
    /// rotated between queueing and running is simply fresh.
    /// </para>
    /// <para>
    /// <c>ProgressionUrl</c> and <c>AnnotationContainer</c> are why this
    /// type exists. They live in the catalogue entry and nowhere else, and
    /// by the time a background download lands the feed is usually gone —
    /// so they are captured here, persisted with the job, and handed to
    /// <see cref="Library.SetSyncTargets"/> after the import.
    /// </para>
    /// </remarks>
    /// <param name="Url">The acquisition. The one field that is not advice.</param>
    /// <param name="Headers">Send these, plus whatever the app sends of its own.</param>
    /// <param name="SuggestedFilename">One safe path component, for a Downloads entry.</param>
    /// <param name="MediaType">What the catalogue claims it is. A UI hint only.</param>
    /// <param name="Title">The entry's title, so progress can name the book.</param>
    /// <param name="EntryId">
    /// The entry's OPDS id: opaque, a key for the app's own job record.
    /// Never build a path out of it.
    /// </param>
    /// <param name="ProgressionUrl">The position-sync service, or null.</param>
    /// <param name="AnnotationContainer">The Web Annotation container, or null.</param>
    public record DownloadRequest(
        string Url,
        IReadOnlyDictionary<string, string> Headers,
        string SuggestedFilename,
        string? MediaType,
        string Title,
        string EntryId,
        string? ProgressionUrl,
        string? AnnotationContainer);

    /// <summary>
    /// Describe an entry's download so the app can run it itself, or
    /// <c>null</c> where the row has nothing to fetch.
    /// </summary>
    /// <remarks>
    /// Cheap and local: it reads the held feed and touches no network. Do
    /// it while the catalogue is open, because the entry is the only place
    /// the sync services exist — see <see cref="DownloadRequest"/>.
    /// </remarks>
    public DownloadRequest? GetDownloadRequest(CatalogEntry entry) =>
        GetDownloadRequest(entry.Index);

    /// <inheritdoc cref="GetDownloadRequest(CatalogEntry)"/>
    public DownloadRequest? GetDownloadRequest(int index)
    {
        nuint at = (nuint)index;
        string? url = Text(at, EntryField.DownloadUrl);
        if (url is null)
        {
            return null;
        }
        return new DownloadRequest(
            url,
            // One constant header, assembled here rather than crossed:
            // catalogue servers negotiate by naive substring match, so
            // this is exactly what the engine would have sent.
            new Dictionary<string, string> { ["Accept"] = "*/*" },
            Text(at, EntryField.DownloadFilename) ?? "book",
            Text(at, EntryField.DownloadMediaType),
            Text(at, EntryField.Title) ?? string.Empty,
            Text(at, EntryField.Id) ?? string.Empty,
            Text(at, EntryField.ProgressionUrl),
            Text(at, EntryField.AnnotationContainer));
    }

    private string? Text(nuint index, EntryField field) =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_catalog_entry_text(Live(), index, field, b, c, out n), nameof(Entries));

    /// <summary>
    /// The ways the held feed can be narrowed. Empty is ordinary.
    /// </summary>
    public IReadOnlyList<CatalogFacet> Facets()
    {
        ChapbookException.Check(
            Interop.cb_catalog_facet_count(Live(), out nuint count), nameof(Facets));
        var facets = new List<CatalogFacet>((int)count);
        for (nuint i = 0; i < count; i++)
        {
            nuint index = i;
            ChapbookException.Check(
                Interop.cb_catalog_facet(Live(), index, out NativeFacet row), nameof(Facets));
            facets.Add(new CatalogFacet(
                FacetText(index, FacetField.Label) ?? string.Empty,
                FacetText(index, FacetField.Group) ?? string.Empty,
                (int)row.Group,
                FacetText(index, FacetField.Href) ?? string.Empty,
                row.Active != 0,
                row.HasCount != 0 ? (long)row.Count : null));
        }
        return facets;
    }

    private string? FacetText(nuint index, FacetField field) =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_catalog_facet_text(Live(), index, field, b, c, out n), nameof(Facets));

    /// <summary>
    /// The next page of a long feed, or <c>null</c> at the end — which is
    /// how a host knows to stop asking.
    /// </summary>
    public string? NextPageUrl => PageUrl(CatalogPage.Next);

    /// <summary>The previous page, or <c>null</c> at the start.</summary>
    public string? PreviousPageUrl => PageUrl(CatalogPage.Previous);

    private string? PageUrl(CatalogPage direction) =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_catalog_page_href(Live(), direction, b, c, out n), nameof(NextPageUrl));

    // ---- Onto the shelf ----

    /// <summary>
    /// Put an entry on the shelf: fetch it, import it into the library at
    /// <paramref name="libraryDirectory"/>, record the sync services it
    /// advertises, and answer with the library row it became.
    /// </summary>
    /// <remarks>
    /// <para>
    /// <b>This is the call the whole class exists for.</b> A book's
    /// position and annotation services live in its catalogue entry and
    /// nowhere else, so a host that downloaded by hand could store sync
    /// targets it had no way to learn — and a book added any other way is
    /// a book that will never reconcile.
    /// </para>
    /// <para>
    /// <b>Blocking, and the slowest call in this binding</b>: it is a
    /// whole book over the network, inside this call — so it is also the
    /// wrong call for a download that must survive the process. For that
    /// use <see cref="GetDownloadRequest(CatalogEntry)"/>, then
    /// <see cref="Library.ImportFile"/> and
    /// <see cref="Library.SetSyncTargets"/>. This is exactly those steps
    /// run back to back, which is why the services have to be read before
    /// a transfer that will outlive the feed.
    /// </para>
    /// <para>
    /// Throws with
    /// <see cref="Status.Unavailable"/> for an entry with nothing to
    /// acquire — a navigation row, or a purchase-only entry whose
    /// <see cref="CatalogEntry.Href"/> belongs in a browser.
    /// </para>
    /// </remarks>
    public long Download(CatalogEntry entry, string libraryDirectory)
    {
        ArgumentNullException.ThrowIfNull(entry);
        return Download(entry.Index, libraryDirectory);
    }

    /// <inheritdoc cref="Download(CatalogEntry, string)"/>
    public long Download(int index, string libraryDirectory)
    {
        ArgumentNullException.ThrowIfNull(libraryDirectory);
        ChapbookException.Check(
            Interop.cb_catalog_download(Live(), (nuint)index, libraryDirectory, out long book),
            nameof(Download));
        return book;
    }

    // ---- The login sheet ----

    /// <summary>
    /// The title of the authentication document from the last refusal —
    /// the catalogue's own name for itself, which belongs at the top of a
    /// login sheet. <c>null</c> when no fetch has been refused.
    /// </summary>
    public string? AuthTitle =>
        Strings.Read((byte[]? b, nuint c, out nuint n) =>
            Interop.cb_catalog_auth_title(Live(), b, c, out n), nameof(AuthTitle));

    /// <summary>
    /// Whether the refusing catalogue offers the username-and-password
    /// flow — the one <see cref="SignIn"/> speaks, and the only one OPDS
    /// defines that a reader can complete without a browser.
    /// </summary>
    /// <remarks>
    /// <c>false</c> means the catalogue wants something else (OAuth,
    /// SAML); a host should say so plainly rather than show a login that
    /// cannot work. Also <c>false</c> when no fetch has been refused.
    /// </remarks>
    public bool AuthOffersBasic
    {
        get
        {
            Status status = Interop.cb_catalog_auth_offers_basic(Live(), out byte offers);
            if (status == Status.Unavailable)
            {
                return false;
            }
            ChapbookException.Check(status, nameof(AuthOffersBasic));
            return offers != 0;
        }
    }

    public void Dispose()
    {
        if (_handle != 0)
        {
            Interop.cb_catalog_close(_handle);
            _handle = 0;
        }
    }
}
