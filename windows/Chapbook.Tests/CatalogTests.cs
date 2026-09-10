using System.Text;
using Chapbook;
using Xunit;

namespace Chapbook.Tests;

/// <summary>
/// A catalogue served from the repository's OPDS fixtures, with no socket
/// anywhere.
/// </summary>
/// <remarks>
/// The Rust ABI test for the catalogue needs a live server and skips
/// without one. This one does not, for the same reason the sync tests do
/// not: the transport <i>is</i> the seam, so a fake one drives the whole
/// path — fetch, drill-in, facets, paging, search, the 401 flow, and a
/// download onto a shelf — against the same wire-format fixtures
/// <c>opds-client</c> parses in its own tests.
/// </remarks>
internal sealed class FixtureCatalogTransport : HttpTransport
{
    internal const string Origin = "http://catalog.test";
    internal const string Root = Origin + "/opds/";
    internal const string Private = Origin + "/private/";
    internal const string Username = "reader";
    internal const string Password = "secret";

    internal List<string> Requested { get; } = new();

    public override void Get(in HttpRequest request, HttpResponseBuilder response)
    {
        Requested.Add(request.Url);
        string url = request.Url;

        if (url == Private)
        {
            // The catalogue wants credentials, and says so properly: a 401
            // carrying an authentication document.
            string expected = "Basic " + Convert.ToBase64String(
                Encoding.UTF8.GetBytes($"{Username}:{Password}"));
            bool signedIn = request.Headers.Any(h =>
                h.Key.Equals("Authorization", StringComparison.OrdinalIgnoreCase)
                && h.Value == expected);
            if (!signedIn)
            {
                Answer(
                    response, 401, "application/opds-authentication+json",
                    File.ReadAllBytes(Fixture.Dir("opds/authentication.opds-auth.json")));
                return;
            }
            Answer(response, 200, Navigation, Fixture.Opds("navigation.atom.xml"));
            return;
        }

        switch (url)
        {
            case Root:
                Answer(response, 200, Navigation, Fixture.Opds("navigation.atom.xml"));
                return;
            case Origin + "/opds/feed/new":
                Answer(response, 200, Acquisition, Fixture.Opds("acquisition.atom.xml"));
                return;
            case Origin + "/opds/opensearch.xml":
                Answer(
                    response, 200, "application/opensearchdescription+xml",
                    Fixture.Opds("opensearch.xml"));
                return;
            case Origin + "/dl/demo/c12.cbz":
                Answer(
                    response, 200, "application/vnd.comicbook+zip",
                    File.ReadAllBytes(Fixture.Dir(Path.Combine("cbz", "minimal.cbz"))));
                return;
        }

        // The OpenSearch template in the fixture points at example.com.
        if (url.StartsWith("https://example.com/opds/search?", StringComparison.Ordinal))
        {
            Answer(response, 200, Acquisition, Fixture.Opds("acquisition.atom.xml"));
            return;
        }

        Answer(response, 404, "text/plain", "no"u8.ToArray());
    }

    public override void Send(
        string method, in HttpRequest request, ReadOnlySpan<byte> body,
        HttpResponseBuilder response)
    {
        Requested.Add($"{method} {request.Url}");
        Answer(response, 405, "text/plain", "a catalogue is read"u8.ToArray());
    }

    private const string Navigation =
        "application/atom+xml;profile=opds-catalog;kind=navigation";

    private const string Acquisition =
        "application/atom+xml;profile=opds-catalog;kind=acquisition";

    private static void Answer(
        HttpResponseBuilder response, ushort status, string contentType, byte[] body)
    {
        response.SetStatus(status);
        response.SetContentType(contentType);
        response.AppendBody(body);
    }
}

public class CatalogTests
{
    private static bool HasOpds => Engine.Capabilities.HasFlag(Capabilities.Opds);

    [Fact]
    public void ACatalogIsBrowsedDrilledIntoAndABookTakenOntoTheShelf()
    {
        if (!HasOpds)
        {
            return;
        }
        var transport = new FixtureCatalogTransport();
        using var catalog = new Catalog(transport);

        // The root is a navigation feed: sections, not books.
        catalog.Fetch(FixtureCatalogTransport.Root);
        Assert.Equal("Example Catalog", catalog.FeedTitle);
        Assert.True(catalog.HasSearch);

        IReadOnlyList<CatalogEntry> root = catalog.Entries();
        Assert.NotEmpty(root);
        CatalogEntry section = root.First(e => e.Kind == EntryKind.Navigation);
        Assert.Equal("New Releases", section.Title);
        Assert.False(section.CanDownload);

        // Drill in: a navigation row's href is the next fetch, and it
        // came back absolute — a host never resolves a URL itself.
        string href = section.Href ?? throw new Xunit.Sdk.XunitException("no href");
        Assert.StartsWith(FixtureCatalogTransport.Origin, href);
        catalog.Fetch(href);
        Assert.Equal("New Releases", catalog.FeedTitle);

        // A book row carries what a list draws.
        IReadOnlyList<CatalogEntry> books = catalog.Entries();
        CatalogEntry gopl = Assert.Single(books, e => e.Title == "The Go Programming Language");
        Assert.Equal(EntryKind.Publication, gopl.Kind);
        Assert.Equal(new[] { "Alan Donovan", "Brian Kernighan" }, gopl.Authors);
        Assert.Equal("The authoritative resource.", gopl.Summary);
        Assert.Equal(FixtureCatalogTransport.Origin + "/covers/gopl-t.jpg", gopl.ThumbnailUrl);
        Assert.True(gopl.CanDownload);
        Assert.True(gopl.IsOpenAccess);

        // The money call: onto the shelf, as the library row it became.
        // The comic's first acquisition is open access, so it is the one
        // that can be taken without a purchase page in the way.
        CatalogEntry comic = Assert.Single(books, e => e.Href?.EndsWith(".cbz") == true);
        Assert.True(comic.CanDownload);
        string shelf = Fixture.Scratch();
        long id = catalog.Download(comic, shelf);
        Assert.True(id > 0, "the download answers with its library row");
        Assert.Contains(FixtureCatalogTransport.Origin + "/dl/demo/c12.cbz", transport.Requested);

        using var library = new Library(shelf);
        ShelfBook book = Assert.Single(library.Books());
        Assert.Equal(id, book.Id);

        // A navigation row has nothing to acquire, and says so by code.
        catalog.Fetch(FixtureCatalogTransport.Root);
        var error = Assert.Throws<ChapbookException>(() => catalog.Download(section, shelf));
        Assert.Equal(Status.Unavailable, error.Status);
    }

    [Fact]
    public void APagedFacetedFeedCrossesWithItsChromeAndCanBeSearched()
    {
        if (!HasOpds)
        {
            return;
        }
        using var catalog = new Catalog(new FixtureCatalogTransport());

        // Nothing fetched yet: no search box, no pages, nothing to list.
        Assert.False(catalog.HasSearch);
        Assert.Null(catalog.NextPageUrl);

        catalog.Fetch(FixtureCatalogTransport.Origin + "/opds/feed/new");

        // Page 2 of a paginated set: both neighbours, absolute.
        Assert.Equal(
            FixtureCatalogTransport.Origin + "/opds/feed/new?page=3", catalog.NextPageUrl);
        Assert.Equal(
            FixtureCatalogTransport.Origin + "/opds/feed/new", catalog.PreviousPageUrl);

        // Three facets across two groups, one of them in force.
        IReadOnlyList<CatalogFacet> facets = catalog.Facets();
        Assert.Equal(3, facets.Count);
        Assert.Equal(2, facets.Select(f => f.Group).Distinct().Count());
        Assert.All(facets, f => Assert.NotEmpty(f.Label));
        Assert.All(facets, f => Assert.StartsWith(FixtureCatalogTransport.Origin, f.Href));
        Assert.Contains(facets, f => f.Active);

        // Searching replaces the feed, so it is the same screen.
        Assert.True(catalog.HasSearch);
        catalog.Search("go");
        Assert.Equal("New Releases", catalog.FeedTitle);
        Assert.NotEmpty(catalog.Entries());
    }

    [Fact]
    public void A401IsAnAnswerWithALoginToDraw()
    {
        if (!HasOpds)
        {
            return;
        }
        using var catalog = new Catalog(new FixtureCatalogTransport());

        // Nothing refused yet: nothing to put on a login sheet.
        Assert.Null(catalog.AuthTitle);
        Assert.False(catalog.AuthOffersBasic);

        var refused = Assert.Throws<ChapbookException>(
            () => catalog.Fetch(FixtureCatalogTransport.Private));
        Assert.Equal(Status.AuthRequired, refused.Status);

        // The authentication document was kept, and it is what the sheet
        // draws: the catalogue's own name, and whether a password will do.
        Assert.Equal("Example Catalog", catalog.AuthTitle);
        Assert.True(catalog.AuthOffersBasic);

        // Sign in — the engine does the base64 — and simply try again.
        catalog.SignIn(FixtureCatalogTransport.Username, FixtureCatalogTransport.Password);
        catalog.Fetch(FixtureCatalogTransport.Private);
        Assert.Equal("Example Catalog", catalog.FeedTitle);

        // A successful fetch clears the refusal.
        Assert.Null(catalog.AuthTitle);
    }

    [Fact]
    public void TheEngineLetsGoOfTheTransportWhenTheCatalogCloses()
    {
        if (!HasOpds)
        {
            return;
        }
        var transport = new FakeTransport();
        var catalog = new Catalog(transport);
        Assert.Equal(0, transport.Released);
        catalog.Dispose();
        Assert.Equal(1, transport.Released);
    }
}
