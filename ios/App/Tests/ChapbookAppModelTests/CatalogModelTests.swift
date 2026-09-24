import Chapbook
import Foundation
import Testing

@testable import ChapbookAppModel

// The networking model without a screen: origins parsed the way a
// credential is keyed, credentials kept in the Keychain, and a catalog
// browsed, refused and signed into against a canned server.

@Test func anOriginDropsThePathAndDefaultPortButKeepsANamedOne() {
    #expect(Credentials.origin(of: "https://Example.org/opds/feed?x=1") == "https://example.org")
    #expect(Credentials.origin(of: "https://example.org:443/opds/") == "https://example.org")
    #expect(Credentials.origin(of: "http://example.org:80/x") == "http://example.org")
    #expect(Credentials.origin(of: "https://example.org:8443/x") == "https://example.org:8443")
    #expect(Credentials.origin(of: "not a url") == nil)
}

@Test func aCredentialSurvivesTheKeychainRoundTripAndIsForgotten() {
    let credentials = Credentials(service: "com.ophymx.chapbook.tests.keychain")
    let origin = "https://cred-test-\(UUID().uuidString).example.org"
    #expect(credentials.get(origin) == nil)
    let header = Credentials.basic(username: "reader", password: "secret")
    #expect(header == "Basic " + Data("reader:secret".utf8).base64EncodedString())
    credentials.set(origin, authorization: header)
    #expect(credentials.get(origin) == header)
    #expect(credentials.origins().contains(origin))
    credentials.set(origin, authorization: "Bearer rotated")
    #expect(credentials.get(origin) == "Bearer rotated")
    credentials.forget(origin)
    #expect(credentials.get(origin) == nil)
}

/// A catalog behind a login, one URLProtocol deep: the root refuses
/// until the origin's credential arrives, then lists one book whose
/// acquisition is the fixture.
final class CatalogStub: URLProtocol {
    static let host = "https://catalog.example.test"
    nonisolated(unsafe) static var seenAuthorization: [String?] = []
    static let lock = NSLock()

    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.host == "catalog.example.test"
    }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    static func feed() -> Data {
        Data(
            """
            <?xml version="1.0"?>
            <feed xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog">
              <id>urn:root</id><title>Test Shelf</title>
              <link rel="search" href="\(host)/search?q={searchTerms}" type="application/atom+xml"/>
              <link rel="next" href="\(host)/page2" type="application/atom+xml;profile=opds-catalog"/>
              <link rel="http://opds-spec.org/facet" href="\(host)/en" title="English" opds:facetGroup="Language" opds:activeFacet="true"/>
              <link rel="http://opds-spec.org/facet" href="\(host)/fr" title="French" opds:facetGroup="Language"/>
              <entry><id>urn:shelf</id><title>A Section</title>
                <link rel="subsection" href="\(host)/section" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
              </entry>
              <entry><id>urn:book:1</id><title>Minimal</title>
                <author><name>Nobody</name></author>
                <link rel="http://opds-spec.org/acquisition/open-access" href="\(host)/books/minimal.epub" type="application/epub+zip"/>
                <link rel="http://opds-spec.org/progression" href="\(host)/progress/1" type="application/json"/>
              </entry>
            </feed>
            """.utf8)
    }

    /// The next page: one navigation row and no more pages, so a row
    /// from page one sits at an index the held feed no longer has a book at.
    static func page2() -> Data {
        Data(
            """
            <?xml version="1.0"?>
            <feed xmlns="http://www.w3.org/2005/Atom">
              <id>urn:page2</id><title>Test Shelf</title>
              <entry><id>urn:more</id><title>More</title>
                <link rel="subsection" href="\(host)/more" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
              </entry>
            </feed>
            """.utf8)
    }

    static func authDocument() -> Data {
        Data(
            """
            {"id":"\(host)/auth","title":"Test Shelf Login",
             "authentication":[{"type":"http://opds-spec.org/auth/basic"}]}
            """.utf8)
    }

    override func startLoading() {
        guard let url = request.url else { return }
        let authorization = request.value(forHTTPHeaderField: "Authorization")
        Self.lock.withLock { Self.seenAuthorization.append(authorization) }
        let (status, contentType, body): (Int, String, Data)
        if authorization == nil {
            (status, contentType, body) = (401, "application/opds-authentication+json", Self.authDocument())
        } else if url.path.hasSuffix(".epub") {
            (status, contentType, body) = (200, "application/epub+zip", try! fixture("epub/minimal.epub"))
        } else if url.path == "/page2" {
            (status, contentType, body) = (200, "application/atom+xml;profile=opds-catalog", Self.page2())
        } else {
            (status, contentType, body) = (200, "application/atom+xml;profile=opds-catalog", Self.feed())
        }
        let response = HTTPURLResponse(
            url: url, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": contentType])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
}

@Test @MainActor func aRefusedCatalogBecomesALoginAndASignInReachesTheFeed() async throws {
    let transfers = URLSessionConfiguration.ephemeral
    transfers.protocolClasses = [CatalogStub.self]
    let (app, dir) = try container("catalog", transfers: transfers)
    defer { try? FileManager.default.removeItem(at: dir) }
    let http = Http(credentials: app.credentials, configuration: transfers)
    let saved = app.catalogs.add(url: "\(CatalogStub.host)/opds/")
    app.credentials.forget(Credentials.origin(of: saved.url)!)
    defer { app.credentials.forget(Credentials.origin(of: saved.url)!) }

    let catalogSession = CatalogSession(transport: http.transport, credentials: app.credentials)
    let vm = CatalogViewModel(
        saved: saved, session: catalogSession, credentials: app.credentials, downloads: app.downloads)

    // A 401 is an answer: the login draws from the authentication document.
    await settle { if case .login = vm.ui { return true } else { return false } }
    guard case .login(let title, let offersBasic, let retry) = vm.ui else {
        Issue.record("no login: \(vm.ui)")
        return
    }
    #expect(title == "Test Shelf Login")
    #expect(offersBasic)

    // Signing in stores the credential by origin and fetches again.
    vm.signIn(username: "reader", password: "secret", retry: retry)
    await settle { if case .feed(let f) = vm.ui { return !f.loading } else { return false } }
    guard case .feed(let feed) = vm.ui else {
        Issue.record("no feed: \(vm.ui)")
        return
    }
    #expect(feed.title == "Test Shelf")
    #expect(feed.hasSearch)
    #expect(feed.nextPage?.path == "/page2")
    #expect(feed.entries.map(\.kind) == [.navigation, .publication])
    #expect(feed.facets.map(\.label) == ["English", "French"])
    #expect(feed.facets.first?.isActive == true)
    #expect(app.credentials.get(Credentials.origin(of: saved.url)!) == Credentials.basic(username: "reader", password: "secret"))

    // The screen pages before the reader taps Get: the held feed is now
    // page two, and the row is from page one. It carries its own
    // download, so the job fetches the book the row named — not whatever
    // page two holds at that index, which is the bug this pins.
    vm.loadMore()
    await settle { if case .feed(let f) = vm.ui { return f.entries.count == 3 && !f.loadingMore } else { return false } }
    vm.download(feed.entries[1])
    await settle { app.downloads.landings == 1 }
    #expect(app.downloads.jobs.first?.request.url.path == "/books/minimal.epub")
    let jobs = app.downloads.jobs
    #expect(jobs.count == 1)
    guard case .landed(let book) = jobs.first?.state else {
        Issue.record("did not land: \(String(describing: jobs.first?.state))")
        return
    }
    #expect(try await app.shelf.books().map(\.id) == [book])
    #expect(try await app.shelf.syncProgressionURL(book: book)?.path == "/progress/1")
    // The credential rode on the transfer, added when the task was made.
    #expect(CatalogStub.lock.withLock { CatalogStub.seenAuthorization.last } != nil)

    // A navigation row pushes a crumb; Back walks it before leaving.
    vm.openEntry(feed.entries[0])
    await settle { if case .feed(let f) = vm.ui { return !f.loading && f.url.path == "/section" } else { return false } }
    #expect(vm.back())
    await settle { if case .feed(let f) = vm.ui { return !f.loading && f.url.path == "/opds/" } else { return false } }
    #expect(!vm.back(), "the root is the last crumb")

    // Tapping Get twice while the first is running is one job; a job
    // that already landed is not in the way, because the import is
    // idempotent and a re-fetch is the reader's to ask for.
    let again = try #require(feed.entries[1].download)
    let before = app.downloads.jobs.count
    app.downloads.enqueue(again)
    app.downloads.enqueue(again)
    #expect(app.downloads.jobs.count == before + 1)
}

@Test @MainActor func aDownloadRequestSurvivesTheTaskDescription() throws {
    let request = Catalog.DownloadRequest(
        url: URL(string: "https://x.test/b.epub")!, headers: ["Accept": "*/*"],
        suggestedFilename: "b.epub", mediaType: "application/epub+zip", title: "B",
        entryID: "urn:b", progressionURL: URL(string: "https://x.test/p"), annotationContainer: nil)
    let described = try #require(Downloads.describe(request))
    let task = URLSession.shared.dataTask(with: URL(string: "https://x.test/")!)
    task.taskDescription = described
    let back = try #require(Downloads.request(of: task))
    #expect(back.url == request.url)
    #expect(back.entryID == request.entryID)
    #expect(back.progressionURL == request.progressionURL)
    #expect(back.annotationContainer == nil)
    #expect(!described.contains("Authorization"), "no credential travels in the description")
}

@Test @MainActor func savedCatalogsAreKeptInOrderAndRemoved() throws {
    let (app, dir) = try container("catalogs")
    defer { try? FileManager.default.removeItem(at: dir) }
    #expect(app.catalogs.all.isEmpty)
    let a = app.catalogs.add(url: " https://a.test/opds/ ")
    let b = app.catalogs.add(url: "https://b.test/opds/", title: "B")
    #expect(app.catalogs.all.map(\.url) == ["https://a.test/opds/", "https://b.test/opds/"])
    app.catalogs.rename(a.id, to: "A")
    #expect(app.catalogs.get(a.id)?.title == "A")
    app.catalogs.remove(b.id)
    #expect(app.catalogs.all.map(\.id) == [a.id])
}
