import Foundation

/// A catalog the reader added: where it is and what it called itself.
public struct SavedCatalog: Codable, Hashable, Sendable, Identifiable {
    public let id: String
    public var title: String
    public let url: String

    public init(id: String, title: String, url: String) {
        self.id = id
        self.title = title
        self.url = url
    }
}

/// The catalogs the reader has added, in the order they were added.
///
/// Nothing here is a secret — a catalog's credential lives in
/// `Credentials` under its origin — so defaults hold the list. The
/// engine has no notion of a saved catalog; the shelf only knows the
/// books that came from one.
@MainActor
public final class Catalogs: ObservableObject {
    private let defaults: UserDefaults
    private static let key = "catalogs"

    @Published public private(set) var all: [SavedCatalog]

    public init(defaults: UserDefaults) {
        self.defaults = defaults
        all = Self.load(defaults)
    }

    public func get(_ id: String) -> SavedCatalog? {
        all.first { $0.id == id }
    }

    /// Add a catalog; `title` may be blank until its feed says what it is
    /// called.
    @discardableResult
    public func add(url: String, title: String = "") -> SavedCatalog {
        let catalog = SavedCatalog(
            id: UUID().uuidString,
            title: title.trimmingCharacters(in: .whitespaces),
            url: url.trimmingCharacters(in: .whitespaces))
        save(all + [catalog])
        return catalog
    }

    public func rename(_ id: String, to title: String) {
        save(all.map { $0.id == id ? SavedCatalog(id: $0.id, title: title, url: $0.url) : $0 })
    }

    public func remove(_ id: String) {
        save(all.filter { $0.id != id })
    }

    private func save(_ list: [SavedCatalog]) {
        if let data = try? JSONEncoder().encode(list) {
            defaults.set(data, forKey: Self.key)
        }
        all = list
    }

    private static func load(_ defaults: UserDefaults) -> [SavedCatalog] {
        guard let data = defaults.data(forKey: key) else { return [] }
        return (try? JSONDecoder().decode([SavedCatalog].self, from: data)) ?? []
    }
}
