import Foundation
import Testing

@testable import Chapbook

// A catalog served from the repository's OPDS fixtures, with no socket
// anywhere. The Rust ABI test for the catalog needs a live server and
// skips without one; this one does not, for the same reason the sync
// tests do not: the transport *is* the seam, so a URLProtocol stub drives
// the whole path — fetch, drill-in, facets, paging, search, the 401 flow,
// and a download onto a shelf — against the same wire-format fixtures
// opds-client parses in its own tests. The download goes through
// `URLSession`'s download task, the way it would on a phone.

private let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // ChapbookTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // Chapbook
    .deletingLastPathComponent()  // ios
    .deletingLastPathComponent()  // repo root
    .appendingPathComponent("fixtures")

private let origin = "http://catalog.test"
private let root = URL(string: "\(origin)/opds/")!
private let newReleases = URL(string: "\(origin)/opds/feed/new")!
private let privateShelf = URL(string: "\(origin)/private/")!
private let username = "reader"
private let password = "secret"

private let navigation = "application/atom+xml;profile=opds-catalog;kind=navigation"
private let acquisition = "application/atom+xml;profile=opds-catalog;kind=acquisition"

private func fixture(_ path: String) -> Data {
    (try? Data(contentsOf: fixtures.appendingPathComponent(path))) ?? Data()
}

private final class RequestLog: @unchecked Sendable {
    private let lock = NSLock()
    private var seen: [String] = []
    func record(_ url: String) { lock.withLock { seen.append(url) } }
    var urls: [String] { lock.withLock { seen } }
    func reset() { lock.withLock { seen = [] } }
}

/// The catalog, one URLProtocol deep. `canInit` says yes to everything,
/// so nothing in these tests can reach a real network.
private final class FixtureCatalog: URLProtocol {
    static let log = RequestLog()

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        guard let url = request.url else { return }
        Self.log.record(url.absoluteString)
        let (status, contentType, body) = answer(url)
        let response = HTTPURLResponse(
            url: url, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": contentType])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    private func answer(_ url: URL) -> (Int, String, Data) {
        if url == privateShelf {
            // The catalog wants credentials, and says so properly: a 401
            // carrying an authentication document.
            let expected =
                "Basic " + Data("\(username):\(password)".utf8).base64EncodedString()
            guard request.value(forHTTPHeaderField: "Authorization") == expected else {
                return (
                    401, "application/opds-authentication+json",
                    fixture("opds/authentication.opds-auth.json")
                )
            }
            return (200, navigation, fixture("opds/navigation.atom.xml"))
        }
        switch url.absoluteString {
        case root.absoluteString:
            return (200, navigation, fixture("opds/navigation.atom.xml"))
        case newReleases.absoluteString:
            return (200, acquisition, fixture("opds/acquisition.atom.xml"))
        case "\(origin)/opds/opensearch.xml":
            return (200, "application/opensearchdescription+xml", fixture("opds/opensearch.xml"))
        case "\(origin)/dl/demo/c12.cbz":
            return (200, "application/vnd.comicbook+zip", fixture("cbz/minimal.cbz"))
        default:
            break
        }
        // The OpenSearch template in the fixture points at example.com.
        if url.absoluteString.hasPrefix("https://example.com/opds/search?") {
            return (200, acquisition, fixture("opds/acquisition.atom.xml"))
        }
        return (404, "text/plain", Data("no".utf8))
    }
}

private func stubbedCatalog() throws -> Catalog {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [FixtureCatalog.self]
    return try Catalog(transport: .urlSession(URLSession(configuration: configuration)))
}

private func scratch(_ name: String) throws -> URL {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent(
            "chapbook-swift-catalog-\(ProcessInfo.processInfo.processIdentifier)-\(name)")
    try? FileManager.default.removeItem(at: dir)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

private let opdsBuilt = Capabilities.current().contains(.opds)

@Test(.enabled(if: opdsBuilt))
func aCatalogIsBrowsedDrilledIntoAndABookTakenOntoTheShelf() throws {
    let shelf = try scratch("shelf")
    defer { try? FileManager.default.removeItem(at: shelf) }
    FixtureCatalog.log.reset()
    let catalog = try stubbedCatalog()

    // The root is a navigation feed: sections, not books.
    try catalog.fetch(root)
    #expect(catalog.feedTitle() == "Example Catalog")
    #expect(try catalog.hasSearch())

    let sections = try catalog.entries()
    let section = try #require(sections.first { $0.kind == .navigation })
    #expect(section.title == "New Releases")
    #expect(!section.canDownload)

    // Drill in: a navigation row's href is the next fetch, and it came
    // back absolute — a host never resolves a URL itself.
    let href = try #require(section.href)
    #expect(href.absoluteString.hasPrefix(origin))
    try catalog.fetch(href)
    #expect(catalog.feedTitle() == "New Releases")

    // A book row carries what a list draws.
    let books = try catalog.entries()
    let gopl = try #require(books.first { $0.title == "The Go Programming Language" })
    #expect(gopl.kind == .publication)
    #expect(gopl.authors == ["Alan Donovan", "Brian Kernighan"])
    #expect(gopl.summary == "The authoritative resource.")
    #expect(gopl.thumbnailURL == URL(string: "\(origin)/covers/gopl-t.jpg"))
    #expect(gopl.canDownload && gopl.isOpenAccess)

    // The money call: onto the shelf, as the library row it became. The
    // comic's first acquisition is open access, so it is the one that can
    // be taken with no purchase page in the way — and it lands through
    // the session's download task, not a GET.
    let comic = try #require(books.first { $0.href?.pathExtension == "cbz" })
    #expect(comic.canDownload)
    let id = try catalog.download(comic, into: shelf)
    #expect(id > 0, "the download answers with its library row")
    #expect(FixtureCatalog.log.urls.contains("\(origin)/dl/demo/c12.cbz"))

    let library = try Library(directory: shelf)
    let shelved = try library.books()
    #expect(shelved.count == 1)
    #expect(shelved.first?.id == id)

    // A navigation row has nothing to acquire, and says so by code.
    try catalog.fetch(root)
    let refused = #expect(throws: ChapbookError.self) {
        try catalog.download(section, into: shelf)
    }
    #expect(refused?.status == C.unavailable)
}

@Test(.enabled(if: opdsBuilt))
func aPagedFacetedFeedCrossesWithItsChromeAndCanBeSearched() throws {
    let catalog = try stubbedCatalog()

    // Nothing fetched yet: no title, no search box, no pages.
    #expect(catalog.feedTitle() == nil)
    #expect(try !catalog.hasSearch())
    #expect(catalog.nextPageURL() == nil)

    try catalog.fetch(newReleases)

    // Page 2 of a paginated set: both neighbours, absolute.
    #expect(catalog.nextPageURL() == URL(string: "\(origin)/opds/feed/new?page=3"))
    #expect(catalog.previousPageURL() == newReleases)

    // Three facets across two groups, one of them in force.
    let facets = try catalog.facets()
    #expect(facets.count == 3)
    #expect(Set(facets.map(\.group)).count == 2)
    #expect(facets.allSatisfy { !$0.label.isEmpty && !$0.groupName.isEmpty })
    #expect(facets.allSatisfy { $0.href?.absoluteString.hasPrefix(origin) == true })
    #expect(facets.contains { $0.isActive })
    #expect(facets.compactMap(\.count) == [80, 40, 100])

    // Searching replaces the feed, so it is the same screen.
    #expect(try catalog.hasSearch())
    try catalog.search("go")
    #expect(catalog.feedTitle() == "New Releases")
    #expect(try !catalog.entries().isEmpty)
}

@Test(.enabled(if: opdsBuilt))
func a401IsAnAnswerWithALoginToDraw() throws {
    let catalog = try stubbedCatalog()

    // Nothing refused yet: nothing to put on a login sheet.
    #expect(catalog.authTitle() == nil)
    #expect(try !catalog.authOffersBasic())

    let refused = #expect(throws: ChapbookError.self) { try catalog.fetch(privateShelf) }
    #expect(refused?.isAuthRequired == true)

    // The authentication document was kept, and it is what the sheet
    // draws: the catalog's own name, and whether a password will do.
    #expect(catalog.authTitle() == "Example Catalog")
    #expect(try catalog.authOffersBasic())

    // Sign in — the engine does the base64 — and simply try again.
    try catalog.signIn(username: username, password: password)
    try catalog.fetch(privateShelf)
    #expect(catalog.feedTitle() == "Example Catalog")

    // A successful fetch clears the refusal.
    #expect(catalog.authTitle() == nil)
}
