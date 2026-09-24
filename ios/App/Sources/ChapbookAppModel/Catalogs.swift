import Chapbook
import Foundation

/// A catalog the reader added: where it is and what it called itself.
/// The id is the library's row, carried as a string because that is
/// what a navigation route holds.
public struct SavedCatalog: Codable, Hashable, Sendable, Identifiable {
    public let id: String
    public var title: String
    public let url: String

    public init(id: String, title: String, url: String) {
        self.id = id
        self.title = title
        self.url = url
    }

    init(_ saved: App.SavedCatalog) {
        self.init(id: String(saved.id), title: saved.title, url: saved.url)
    }
}

/// The catalogs the reader has added, in the order they were added, as
/// the engine keeps them beside the shelf. A catalog's credential lives
/// in `Credentials` under its origin, never here.
@MainActor
public final class Catalogs: ObservableObject {
    private let app: App

    @Published public private(set) var all: [SavedCatalog] = []

    public init(app: App) {
        self.app = app
        refresh()
    }

    public func get(_ id: String) -> SavedCatalog? {
        all.first { $0.id == id }
    }

    /// Add a catalog; `title` may be blank until its feed says what it is
    /// called. The engine drops the whitespace a reader types.
    @discardableResult
    public func add(url: String, title: String = "") -> SavedCatalog {
        let added = (try? app.addCatalog(url: url, title: title))
            .map(SavedCatalog.init)
            ?? SavedCatalog(id: "0", title: title, url: url)
        refresh()
        return added
    }

    public func rename(_ id: String, to title: String) {
        if let row = Int64(id) { try? app.renameCatalog(row, to: title) }
        refresh()
    }

    public func remove(_ id: String) {
        if let row = Int64(id) { try? app.removeCatalog(row) }
        refresh()
    }

    private func refresh() {
        all = ((try? app.catalogs()) ?? []).map(SavedCatalog.init)
    }
}
