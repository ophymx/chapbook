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

/// A browsing session over one saved catalog, as a screen sees it.
///
/// The decisions are the engine's: a navigation row pushes a crumb, Back
/// walks the crumbs before it leaves the screen, a facet replaces and a
/// page appends, a 401 is a login rather than a failure, and a sign-in
/// stores the credential by origin and fetches again. This turns each of
/// those into a hop onto the catalog's queue and a snapshot of what it
/// holds afterwards. The one thing mirrored here is the crumb depth, so
/// Back can answer at once whether it stays inside the catalog.
@MainActor
public final class CatalogViewModel: ObservableObject {
    @Published public private(set) var ui: CatalogUI = .opening

    public let saved: SavedCatalog
    private let session: CatalogSession
    private let downloads: Downloads
    /// How many crumbs the engine's trail holds — every `go` pushes one.
    private var depth = 0

    public init(saved: SavedCatalog, session: CatalogSession, downloads: Downloads) {
        self.saved = saved
        self.session = session
        self.downloads = downloads
        if let root = URL(string: saved.url) {
            go(root)
        } else {
            ui = .failed(URL(fileURLWithPath: "/"))
        }
    }

    /// What the catalog holds, as the screen draws it.
    nonisolated private static func snapshot(_ catalog: Catalog, url: URL, fallback: String) throws -> CatalogUI {
        switch try catalog.browseState() {
        case .feed:
            let title = catalog.browseTitle()
            return .feed(
                Browsing(
                    url: catalog.browseURL() ?? url,
                    title: title.isEmpty ? fallback : title,
                    entries: try catalog.entries(),
                    facets: try catalog.facets(),
                    hasSearch: try catalog.hasSearch(),
                    nextPage: catalog.nextPageURL(),
                    loading: false))
        case .login(let title, let offersBasic, let retry):
            return .login(title: title.isEmpty ? fallback : title, offersBasic: offersBasic, retry: retry ?? url)
        case .failed(let failed, _):
            return .failed(failed ?? url)
        case .opening:
            return .failed(url)
        }
    }

    /// Open a feed, pushing a crumb.
    private func go(_ url: URL) {
        depth += 1
        ui = .feed(Browsing(url: url, loading: true))
        let fallback = saved.title
        Task { [weak self] in
            guard let self else { return }
            ui = (try? await session.use { catalog in
                try? catalog.go(url)
                return try Self.snapshot(catalog, url: url, fallback: fallback)
            }) ?? .failed(url)
        }
    }

    public func openEntry(_ entry: Catalog.Entry) {
        guard entry.kind == .navigation, let href = entry.href else { return }
        go(href)
    }

    /// Enqueue a publication's download. The entry carries its own
    /// request, captured when its row was read, so this needs neither the
    /// catalog nor the feed it came from — which may be pages back.
    public func download(_ entry: Catalog.Entry) {
        guard let request = entry.download else { return }
        downloads.enqueue(request)
    }

    public func applyFacet(_ facet: Catalog.Facet) {
        guard case .feed(var feed) = ui, let index = feed.facets.firstIndex(of: facet) else { return }
        depth += 1
        feed.loading = true
        ui = .feed(feed)
        let url = facet.href ?? feed.url
        let fallback = saved.title
        Task { [weak self] in
            guard let self else { return }
            ui = (try? await session.use { catalog in
                try? catalog.applyFacet(at: index)
                return try Self.snapshot(catalog, url: url, fallback: fallback)
            }) ?? .failed(url)
        }
    }

    public func search(_ query: String) {
        guard case .feed(var feed) = ui else { return }
        feed.loading = true
        ui = .feed(feed)
        let url = feed.url
        let fallback = saved.title
        Task { [weak self] in
            guard let self else { return }
            ui = (try? await session.use { catalog in
                try? catalog.search(query)
                return try Self.snapshot(catalog, url: url, fallback: fallback)
            }) ?? .failed(url)
        }
    }

    public func loadMore() {
        guard case .feed(var feed) = ui, feed.nextPage != nil, !feed.loadingMore else { return }
        feed.loadingMore = true
        ui = .feed(feed)
        Task { [weak self] in
            guard let self else { return }
            let more: (entries: [Catalog.Entry], next: URL?)? = try? await session.use { catalog in
                guard (try? catalog.loadMore()) == true else { return nil }
                return (try catalog.entries(), catalog.nextPageURL())
            }
            guard case .feed(var current) = ui else { return }
            current.loadingMore = false
            if let more {
                current.entries = more.entries
                current.nextPage = more.next
            }
            ui = .feed(current)
        }
    }

    /// Sign in with Basic: the engine stores it by origin and fetches the
    /// refused feed again, moving no crumb.
    public func signIn(username: String, password: String, retry: URL) {
        ui = .feed(Browsing(url: retry, loading: true))
        let fallback = saved.title
        Task { [weak self] in
            guard let self else { return }
            ui = (try? await session.use { catalog in
                try? catalog.submitLogin(username: username, password: password)
                return try Self.snapshot(catalog, url: retry, fallback: fallback)
            }) ?? .failed(retry)
        }
    }

    /// True when Back stayed inside the catalog; false when the screen
    /// should close.
    public func back() -> Bool {
        guard depth > 1 else { return false }
        depth -= 1
        if case .feed(var feed) = ui {
            feed.loading = true
            ui = .feed(feed)
        }
        let fallback = saved.title
        let root = URL(string: saved.url) ?? URL(fileURLWithPath: "/")
        Task { [weak self] in
            guard let self else { return }
            ui = (try? await session.use { catalog in
                _ = try catalog.back()
                return try Self.snapshot(catalog, url: catalog.browseURL() ?? root, fallback: fallback)
            }) ?? .failed(root)
        }
        return true
    }
}
