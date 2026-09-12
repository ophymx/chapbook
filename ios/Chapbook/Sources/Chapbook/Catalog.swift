import CChapbook
import Foundation

/// An OPDS catalog, browsed one feed at a time.
///
/// **Every call that touches the network blocks** — `fetch(_:)`,
/// `search(_:)` and above all `download(_:into:)`. Run them off the main
/// actor, in a `Task` whose cancellation is the app's own; this binding
/// deliberately invents no worker, because Swift's concurrency is better
/// than anything it could.
///
/// The transport is the app's, on the same terms as everywhere else — the
/// same choice a `SessionConfiguration` makes, with the same default:
/// `URLSession` on iOS, the bundled Rust transport on macOS. A catalog
/// behind a proxy, a user CA or a corporate network works because the
/// app's `URLSession` does. A `URLSession` also brings its download
/// facility, which is what a whole book over cellular wants.
///
/// **A 401 is an answer.** A fetch the catalog refuses for want of
/// credentials throws a `ChapbookError` whose `isAuthRequired` is true,
/// and keeps the authentication document: `authTitle()` and
/// `authOffersBasic()` are what a login sheet draws, `signIn(username:
/// password:)` is what it submits, and the fetch is then simply tried
/// again.
///
/// Like `Session`, deliberately **not** `Sendable`: the handle is movable
/// between isolation domains and never shareable across them.
public final class Catalog {
    let raw: OpaquePointer

    /// What a row is, which decides what tapping it does.
    public enum EntryKind: Sendable {
        /// A place to go: a shelf, a section, another feed. Tapping it
        /// fetches the entry's `href`.
        case navigation
        /// A book. Tapping it downloads.
        case publication
    }

    /// One row of a catalog listing.
    ///
    /// Images cross as URLs, never bytes — a cover grid is what the
    /// platform's image loader is for. Send the same `Authorization`
    /// with them if the catalog wants one.
    public struct Entry: Hashable, Sendable {
        /// The row's index in the held feed, which is what a download
        /// takes. Valid until the next `fetch(_:)` or `search(_:)`.
        public let index: Int
        public let kind: EntryKind
        public let title: String
        public let authors: [String]
        /// Plain text. HTML descriptions are deliberately not offered: a
        /// host would have to sanitize what it did not parse.
        public let summary: String?
        public let publisher: String?
        /// The language tag the catalog states.
        public let language: String?
        public let series: String?
        /// Where in its series, when the entry says. Only ever set
        /// alongside `series`.
        public let seriesPosition: Double?
        /// Absolute, for a list row.
        public let thumbnailURL: URL?
        /// Absolute, for a detail screen.
        public let coverURL: URL?
        /// Where tapping goes: the feed a navigation row points at, or a
        /// publication's acquisition. A commercial entry whose only link
        /// is a purchase page answers with that page, which a host opens
        /// in a browser rather than downloading.
        public let href: URL?
        /// Whether `download(_:into:)` can take this row at all. A
        /// purchase-only entry answers false.
        public let canDownload: Bool
        /// Freely downloadable, as against borrowed or bought. What tells
        /// a "Get" button from a "Buy" one.
        public let isOpenAccess: Bool
        /// Whether the entry advertises a position-sync service. A host
        /// does not have to act on it — the download records it — but a
        /// catalog that syncs is worth saying so on the row.
        public let syncsPosition: Bool
        /// Likewise, an annotation container.
        public let syncsAnnotations: Bool
    }

    /// One facet: a way to narrow the held feed, as the catalog offers
    /// it.
    public struct Facet: Hashable, Sendable {
        /// The facet's own label — "English", "By title".
        public let label: String
        /// Its group's name — "Language", "Sort by".
        public let groupName: String
        /// Which group it belongs to, as an index. Facets in a group are
        /// alternatives; a host draws one control per group.
        public let group: Int
        /// The URL that applies it; hand it to `fetch(_:)`.
        public let href: URL?
        /// Whether this facet is the one currently in force.
        public let isActive: Bool
        /// How many entries it would show, when the catalog says.
        public let count: Int?
    }

    /// Open a catalog client over `transport`.
    ///
    /// The `URLSession` path installs both halves the engine can use: the
    /// GET every feed rides on, and a download straight to a file for the
    /// book itself. On failure the engine has already run the transport's
    /// finalizer — its ownership rule — so there is nothing to release.
    public init(transport: HTTPTransport = .platformDefault) throws {
        var handle: OpaquePointer?
        let status: Int32
        switch transport.kind {
        case .bundled:
            // All null asks for the bundled transport; the open declines
            // honestly in a build without one.
            status = cb_catalog_open(nil, nil, nil, nil, &handle)
        case .urlSession(let session):
            let box = Unmanaged.passRetained(URLSessionTransport(session: session))
            status = cb_catalog_open(
                transportGet, transportDownload, transportFinalize, box.toOpaque(), &handle)
        }
        try check(status)
        guard let handle else { throw ChapbookError.openFailure() }
        raw = handle
    }

    deinit { cb_catalog_close(raw) }

    // MARK: Credentials

    /// Send this `Authorization` header value with every request — a
    /// bearer token, or whatever the catalog's own scheme wants. `nil`
    /// clears it.
    ///
    /// The value is opaque and never parsed. Key any store you keep it in
    /// by *origin*, not by the catalog URL: a catalog URL's path can
    /// itself be a secret.
    public func setAuthorization(_ value: String?) throws {
        try withOptionalCString(value) { try check(cb_catalog_set_authorization(raw, $0)) }
    }

    /// Sign in with a username and password — the HTTP Basic flow, which
    /// is what an OPDS authentication document offers when it offers
    /// anything. The encoding is done in the engine on purpose, so no
    /// host has to guess whether the credential is UTF-8 first.
    public func signIn(username: String, password: String) throws {
        try check(cb_catalog_set_basic_auth(raw, username, password))
    }

    // MARK: Browsing

    /// Fetch what is at `url` and hold it — a catalog root, a section a
    /// navigation row pointed at, a facet's narrowing, a page of a long
    /// feed. Whatever was held before is replaced.
    ///
    /// **Blocking.** Throws with `isAuthRequired` when the catalog wants
    /// credentials and said so properly; the authentication document is
    /// then held for `authTitle()` and `authOffersBasic()`.
    public func fetch(_ url: URL) throws {
        try check(cb_catalog_fetch(raw, url.absoluteString))
    }

    /// Search the catalog that is currently held. The results replace it,
    /// so browsing and searching are the same screen.
    ///
    /// Throws `CB_ERR_UNAVAILABLE` when this catalog offers no search —
    /// `hasSearch()` is worth asking before drawing a box. Blocking, like
    /// the fetch.
    public func search(_ query: String) throws {
        try check(cb_catalog_search(raw, query))
    }

    /// The held feed's title — what a browse screen puts at the top.
    /// `nil` before anything is fetched.
    public func feedTitle() -> String? {
        readString { cb_catalog_feed_title(raw, $0, $1, $2) }
    }

    /// Whether this catalog offers a search — what decides if a search
    /// box is drawn at all. `false` before anything is fetched.
    public func hasSearch() throws -> Bool {
        var has = false
        try check(cb_catalog_has_search(raw, &has))
        return has
    }

    /// Every row of the held feed.
    public func entries() throws -> [Entry] {
        var count = 0
        try check(cb_catalog_entry_count(raw, &count))
        var entries: [Entry] = []
        entries.reserveCapacity(count)
        for index in 0..<count {
            var row = cb_entry()
            try check(cb_catalog_entry(raw, index, &row))
            var authors: [String] = []
            authors.reserveCapacity(row.author_count)
            for author in 0..<row.author_count {
                authors.append(
                    try readOptionalString {
                        cb_catalog_entry_author(raw, index, author, $0, $1, $2)
                    } ?? "")
            }
            entries.append(
                Entry(
                    index: index,
                    kind: row.kind == CB_ENTRY_PUBLICATION ? .publication : .navigation,
                    title: try text(index, CB_ENTRY_TITLE) ?? "",
                    authors: authors,
                    summary: try text(index, CB_ENTRY_SUMMARY),
                    publisher: try text(index, CB_ENTRY_PUBLISHER),
                    language: try text(index, CB_ENTRY_LANGUAGE),
                    series: try text(index, CB_ENTRY_SERIES),
                    seriesPosition: row.has_series && row.has_series_position
                        ? row.series_position : nil,
                    thumbnailURL: try text(index, CB_ENTRY_THUMBNAIL_URL).flatMap(URL.init),
                    coverURL: try text(index, CB_ENTRY_COVER_URL).flatMap(URL.init),
                    href: try text(index, CB_ENTRY_HREF).flatMap(URL.init),
                    canDownload: row.can_download,
                    isOpenAccess: row.is_open_access,
                    syncsPosition: row.syncs_position,
                    syncsAnnotations: row.syncs_annotations))
        }
        return entries
    }

    /// One of an entry's strings, `nil` where the entry carries none.
    private func text(_ index: Int, _ field: cb_entry_field) throws -> String? {
        try readOptionalString { cb_catalog_entry_text(raw, index, field, $0, $1, $2) }
    }

    /// The ways the held feed can be narrowed. Empty is ordinary.
    public func facets() throws -> [Facet] {
        var count = 0
        try check(cb_catalog_facet_count(raw, &count))
        var facets: [Facet] = []
        facets.reserveCapacity(count)
        for index in 0..<count {
            var row = cb_facet()
            try check(cb_catalog_facet(raw, index, &row))
            facets.append(
                Facet(
                    label: try facetText(index, CB_FACET_LABEL) ?? "",
                    groupName: try facetText(index, CB_FACET_GROUP) ?? "",
                    group: row.group,
                    href: try facetText(index, CB_FACET_HREF).flatMap(URL.init),
                    isActive: row.active,
                    count: row.has_count ? Int(clamping: row.count) : nil))
        }
        return facets
    }

    private func facetText(_ index: Int, _ field: cb_facet_field) throws -> String? {
        try readOptionalString { cb_catalog_facet_text(raw, index, field, $0, $1, $2) }
    }

    /// The next page of a long feed, or `nil` at the end — which is how a
    /// host knows to stop asking.
    public func nextPageURL() -> URL? {
        pageURL(CB_PAGE_NEXT)
    }

    /// The previous page, or `nil` at the start.
    public func previousPageURL() -> URL? {
        pageURL(CB_PAGE_PREVIOUS)
    }

    private func pageURL(_ direction: cb_catalog_page) -> URL? {
        readString { cb_catalog_page_href(raw, direction, $0, $1, $2) }.flatMap(URL.init)
    }

    // MARK: Onto the shelf

    /// Put an entry on the shelf: fetch it, import it into the library at
    /// `libraryDirectory`, record the sync services it advertises, and
    /// answer with the library row it became — `Library.Book.id`'s space.
    ///
    /// **This is the call the whole class exists for.** A book's position
    /// and annotation services live in its catalog entry and nowhere
    /// else, so a host that downloaded by hand could store sync targets
    /// it had no way to learn — and a book added any other way is a book
    /// that will never reconcile.
    ///
    /// **Blocking, and the slowest call in this package**: it is a whole
    /// book over the network. Throws `CB_ERR_UNAVAILABLE` for an entry
    /// with nothing to acquire — a navigation row, or a purchase-only
    /// entry whose `href` belongs in a browser. The staging file is
    /// removed whatever happens; the library keeps its own copy.
    public func download(_ entry: Entry, into libraryDirectory: URL) throws -> Int64 {
        try download(entryAt: entry.index, into: libraryDirectory)
    }

    /// `download(_:into:)` by index, for a host that kept only that.
    public func download(entryAt index: Int, into libraryDirectory: URL) throws -> Int64 {
        var book: Int64 = 0
        try check(cb_catalog_download(raw, index, libraryDirectory.path, &book))
        return book
    }

    // MARK: The login sheet

    /// The title of the authentication document from the last refusal —
    /// the catalog's own name for itself, which belongs at the top of a
    /// login sheet. `nil` when no fetch has been refused.
    public func authTitle() -> String? {
        readString { cb_catalog_auth_title(raw, $0, $1, $2) }
    }

    /// Whether the refusing catalog offers the username-and-password flow
    /// — the one `signIn(username:password:)` speaks, and the only one
    /// OPDS defines that a reader can complete without a browser.
    ///
    /// `false` means the catalog wants something else (OAuth, SAML); a
    /// host should say so plainly rather than show a login that cannot
    /// work. Also `false` when no fetch has been refused.
    public func authOffersBasic() throws -> Bool {
        var offers = false
        try check(cb_catalog_auth_offers_basic(raw, &offers))
        return offers
    }
}
