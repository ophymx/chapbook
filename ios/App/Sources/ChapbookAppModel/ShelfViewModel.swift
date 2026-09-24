import Chapbook
import Combine
import Foundation

/// What the shelf screen draws.
public struct ShelfState: Sendable {
    public var books: [Library.Book] = []
    public var search = ""
    public var sort: ShelfSort = .read
    public var state: ReadingState? = nil
    public var loading = true
    /// Something to tell the reader once, or `nil`.
    public var notice: Notice? = nil
}

/// A one-line message for the reader; the screen decides the words.
public enum Notice: Sendable {
    case openFailed
}

/// The shelf's decisions: which books, in what order, and what adding
/// one does. The default sort is *recently read*, which is what the
/// desktop app and the CLI answer too — a shelf opens on the book the
/// reader was in.
@MainActor
public final class ShelfViewModel: ObservableObject {
    @Published public private(set) var state = ShelfState()

    private let shelf: Shelf
    private let opener: Opener
    private let downloads: Downloads
    private var following: AnyCancellable?

    public init(shelf: Shelf, opener: Opener, downloads: Downloads) {
        self.shelf = shelf
        self.opener = opener
        self.downloads = downloads
        refresh()
        // A download that lands adds a book; the shelf follows it.
        following = downloads.$landings.dropFirst().sink { [weak self] _ in self?.refresh() }
    }

    /// How many downloads are running, for a shelf badge.
    public var downloading: Int { downloads.active }

    public func refresh() {
        let query = Library.Query(
            search: state.search.trimmingCharacters(in: .whitespaces).isEmpty
                ? nil : state.search.trimmingCharacters(in: .whitespaces),
            state: state.state, sort: state.sort)
        Task { [weak self] in
            guard let self else { return }
            let books = (try? await shelf.books(query)) ?? []
            self.state.books = books
            self.state.loading = false
        }
    }

    public func setSearch(_ search: String) {
        state.search = search
        refresh()
    }

    public func setSort(_ sort: ShelfSort) {
        state.sort = sort
        refresh()
    }

    public func setStateFilter(_ filter: ReadingState?) {
        state.state = filter
        refresh()
    }

    /// Add a file and, if it is a book, say which row so the caller can
    /// open it.
    public func add(_ url: URL, onAdded: @escaping (Int64) -> Void) {
        Task { [weak self] in
            guard let self else { return }
            switch await opener.add(url) {
            case .book(let id):
                refresh()
                onAdded(id)
            case .failed(let reason):
                EngineLog.write(reason, level: .warn, target: "shelf")
                state.notice = .openFailed
            }
        }
    }

    public func setFinished(_ book: Library.Book, finished: Bool) {
        Task { [weak self] in
            guard let self else { return }
            try? await shelf.setFinished(finished, book: book.id)
            refresh()
        }
    }

    public func remove(_ book: Library.Book) {
        Task { [weak self] in
            guard let self else { return }
            try? await shelf.remove(book: book.id)
            refresh()
        }
    }

    public func dismissNotice() {
        state.notice = nil
    }
}
