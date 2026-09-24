import Chapbook
import Foundation

/// One feed on screen, and the crumb trail that led to it.
public struct Browsing: Sendable {
    public var url: URL
    public var title = ""
    public var entries: [Catalog.Entry] = []
    public var facets: [Catalog.Facet] = []
    public var hasSearch = false
    public var nextPage: URL? = nil
    public var loading = true
    /// Rows appended by paging, held apart so a facet change can replace
    /// cleanly.
    public var loadingMore = false
}

public enum CatalogUI: Sendable {
    case opening
    case feed(Browsing)
    /// A 401 with a login to draw.
    case login(title: String, offersBasic: Bool, retry: URL)
    case failed(URL)
}

/// A browsing session over one saved catalog.
///
/// The `CatalogSession` owns the blocking catalog on its own queue; this
/// turns feeds into screens and taps into fetches. A navigation row is a
/// fetch that pushes a crumb; a publication row is a download the app
/// enqueues; a facet or a page is a fetch that replaces or appends. A
/// 401 surfaces as a login rather than a failure, and a sign-in stores
/// the credential by origin and fetches again.
@MainActor
public final class CatalogViewModel: ObservableObject {
    @Published public private(set) var ui: CatalogUI = .opening

    public let saved: SavedCatalog
    private let session: CatalogSession
    private let credentials: Credentials
    private let downloads: Downloads
    private var crumbs: [URL] = []

    public init(saved: SavedCatalog, session: CatalogSession, credentials: Credentials, downloads: Downloads) {
        self.saved = saved
        self.session = session
        self.credentials = credentials
        self.downloads = downloads
        if let root = URL(string: saved.url) {
            open(root)
        } else {
            ui = .failed(URL(fileURLWithPath: "/"))
        }
    }

    private func open(_ url: URL, pushCrumb: Bool = true) {
        if pushCrumb { crumbs.append(url) }
        ui = .feed(Browsing(url: url, loading: true))
        Task { [weak self] in
            guard let self else { return }
            ui = await fetch(url)
        }
    }

    private func fetch(_ url: URL) async -> CatalogUI {
        let fallback = saved.title
        do {
            return try await session.use(for: url) { catalog in
                try catalog.fetch(url)
                return CatalogUI.feed(try Self.read(catalog, url: url, fallback: fallback))
            }
        } catch let error as ChapbookError where error.isAuthRequired {
            return (try? await session.use(for: url) { catalog in
                CatalogUI.login(
                    title: catalog.authTitle() ?? fallback,
                    offersBasic: (try? catalog.authOffersBasic()) ?? false,
                    retry: url)
            }) ?? .failed(url)
        } catch {
            return .failed(url)
        }
    }

    nonisolated private static func read(_ catalog: Catalog, url: URL, fallback: String) throws -> Browsing {
        Browsing(
            url: url,
            title: catalog.feedTitle() ?? fallback,
            entries: try catalog.entries(),
            facets: try catalog.facets(),
            hasSearch: try catalog.hasSearch(),
            nextPage: catalog.nextPageURL(),
            loading: false)
    }

    public func openEntry(_ entry: Catalog.Entry) {
        guard entry.kind == .navigation, let href = entry.href else { return }
        open(href)
    }

    /// Enqueue a publication's download. The entry carries its own
    /// request, captured when its row was read, so this needs neither the
    /// catalog nor the feed it came from — which may be pages back.
    public func download(_ entry: Catalog.Entry) {
        guard let request = entry.download else { return }
        downloads.enqueue(request)
    }

    public func applyFacet(_ facet: Catalog.Facet) {
        guard let href = facet.href else { return }
        open(href)
    }

    public func search(_ query: String) {
        guard case .feed(var feed) = ui else { return }
        feed.loading = true
        ui = .feed(feed)
        let url = feed.url
        let fallback = saved.title
        Task { [weak self] in
            guard let self else { return }
            ui = (try? await session.use(for: url) { catalog in
                try catalog.search(query)
                return CatalogUI.feed(try Self.read(catalog, url: url, fallback: fallback))
            }) ?? .failed(url)
        }
    }

    public func loadMore() {
        guard case .feed(var feed) = ui, let next = feed.nextPage, !feed.loadingMore else { return }
        feed.loadingMore = true
        ui = .feed(feed)
        Task { [weak self] in
            guard let self else { return }
            let more: (entries: [Catalog.Entry], next: URL?)? = try? await session.use(for: next) { catalog in
                try catalog.fetch(next)
                return (try catalog.entries(), catalog.nextPageURL())
            }
            guard case .feed(var current) = ui else { return }
            current.loadingMore = false
            if let more {
                current.entries += more.entries
                current.nextPage = more.next
            }
            ui = .feed(current)
        }
    }

    /// Sign in with Basic, store it by origin, and fetch the refused feed
    /// again.
    public func signIn(username: String, password: String, retry: URL) {
        if let origin = Credentials.origin(of: retry) {
            credentials.set(origin, authorization: Credentials.basic(username: username, password: password))
        }
        open(retry, pushCrumb: false)
    }

    /// True when Back stayed inside the catalog; false when the screen
    /// should close.
    public func back() -> Bool {
        guard crumbs.count > 1 else { return false }
        crumbs.removeLast()
        open(crumbs[crumbs.count - 1], pushCrumb: false)
        return true
    }
}
