import CChapbook
import Foundation

/// How far through a book the reader is, as a shelf groups it.
///
/// Deliberately not derived from progress: a book skimmed to the last
/// page reads 1.0 without being finished, and a finished book reopened
/// reads near 0 without being unread.
public enum ReadingState: Sendable {
    /// Never opened.
    case unread
    /// Opened, not finished — what a "continue reading" row wants.
    case reading
    /// Reached the end at least once, whatever the position says now.
    case finished

    var code: UInt32 {
        switch self {
        case .unread: UInt32(CB_STATE_UNREAD.rawValue)
        case .reading: UInt32(CB_STATE_READING.rawValue)
        case .finished: UInt32(CB_STATE_FINISHED.rawValue)
        }
    }

    init?(code: UInt32) {
        switch code {
        case UInt32(CB_STATE_UNREAD.rawValue): self = .unread
        case UInt32(CB_STATE_READING.rawValue): self = .reading
        case UInt32(CB_STATE_FINISHED.rawValue): self = .finished
        default: return nil
        }
    }
}

/// How a query orders its rows.
public enum ShelfSort: Sendable {
    /// Newest addition first: a stable listing.
    case added
    /// The most recent thing that happened, read or added — what a shelf
    /// shows first.
    case read
    case title
    /// First author, then title. A book with no author sorts last.
    case author
    /// Series, then position within it. Books in no series sort last.
    case series

    var code: UInt32 {
        switch self {
        case .added: UInt32(CB_SORT_ADDED.rawValue)
        case .read: UInt32(CB_SORT_READ.rawValue)
        case .title: UInt32(CB_SORT_TITLE.rawValue)
        case .author: UInt32(CB_SORT_AUTHOR.rawValue)
        case .series: UInt32(CB_SORT_SERIES.rawValue)
        }
    }
}

/// The shelf: the books this app has opened, and the groupings over them.
///
/// Like `Session`, deliberately **not** `Sendable` — the handle is `Send`
/// and not `Sync`, which is what a non-`Sendable` class expresses under
/// strict concurrency. Unlike `Session` it may be held *while* a session
/// is open: the database is WAL, and two connections is the ordinary way
/// to draw a shelf while a book is being read.
///
/// A book reaches the library by being **opened**, not by being imported
/// here. `Session` records it, and `Session.bookID()` says which row that
/// became — so an app's "add to library" is a read, and this is what
/// browses the result.
public final class Library {
    let raw: OpaquePointer

    /// Open (creating if needed) the library at `directory`.
    ///
    /// `nil` asks for this platform's own location, which iOS does not
    /// have: a sandbox knows its own container and has to say it, so pass
    /// the same URL a `SessionConfiguration` was given. A session that
    /// wrote its position somewhere else is a session whose book this
    /// shelf will not list.
    public init(directory: URL? = nil) throws {
        var handle: OpaquePointer?
        let status =
            if let directory {
                cb_library_open(directory.path, &handle)
            } else {
                cb_library_open(nil, &handle)
            }
        try check(status)
        guard let handle else { throw ChapbookError.openFailure() }
        raw = handle
    }

    deinit { cb_library_close(raw) }

    /// Where this platform keeps a per-user library, if it has an answer.
    /// `nil` on iOS, and that is not a gap — see `init`.
    public static var defaultDirectory: URL? {
        readString { cb_library_default_dir($0, $1, $2) }.map { URL(fileURLWithPath: $0) }
    }

    // MARK: Browsing

    /// What to list, and in what order. The defaults are the whole shelf.
    public struct Query: Sendable {
        /// Free text over title, authors and series. Whole words matched
        /// by prefix and folded for case *and* accents — "bronte" finds
        /// Brontë. Text with nothing searchable in it (punctuation alone)
        /// matches no book rather than every book, so an empty result is
        /// not a filter that was ignored.
        public var search: String?
        public var collection: Int64?
        /// Matched exactly but case-folded; the value comes from a row,
        /// not from typing.
        public var series: String?
        public var state: ReadingState?
        public var sort: ShelfSort
        public var limit: Int?
        public var offset: Int

        public init(
            search: String? = nil,
            collection: Int64? = nil,
            series: String? = nil,
            state: ReadingState? = nil,
            sort: ShelfSort = .added,
            limit: Int? = nil,
            offset: Int = 0
        ) {
            self.search = search
            self.collection = collection
            self.series = series
            self.state = state
            self.sort = sort
            self.limit = limit
            self.offset = offset
        }
    }

    /// A collection a book is in, as a row names them.
    public struct CollectionRef: Hashable, Sendable, Identifiable {
        public let id: Int64
        public let name: String
    }

    /// A collection as a list of collections shows them.
    public struct Collection: Hashable, Sendable, Identifiable {
        public let id: Int64
        public let name: String
        public let addedAt: Date
        /// A list of shelf names that does not say how many books are on
        /// each is a list of words.
        public let bookCount: Int
    }

    /// One book on the shelf.
    public struct Book: Hashable, Sendable, Identifiable {
        public let id: Int64
        public let title: String
        public let authors: [String]
        public let series: String?
        /// Fractional on purpose: a novella between books two and three
        /// is conventionally 2.5.
        public let seriesIndex: Double?
        public let language: String?
        public let identifier: String?
        /// SHA-1 of the file's bytes, hex — the key to map a
        /// security-scoped bookmark to, because it identifies the *file*
        /// across a reinstall while `id` identifies the reader's history
        /// of it.
        public let fingerprint: String
        /// The library's own copy, or `nil` for a book it holds a record
        /// of and no copy of — the platform owns the file and the app
        /// owns the bookmark that reaches it.
        public let fileURL: URL?
        /// Kept at import, so a shelf need not reopen every book to draw
        /// one.
        public let coverURL: URL?
        public let collections: [CollectionRef]
        public let state: ReadingState
        /// How far through, 0...1. Draw the bar from this and the badge
        /// from `state`; they answer different questions.
        public let progress: Double?
        public let addedAt: Date
        public let lastRead: Date?
        public let finishedAt: Date?
    }

    /// Run a query and read its rows.
    ///
    /// The rows are copied out here rather than left behind a cursor,
    /// which is the point: a search field issues a query on every
    /// keystroke and the list being drawn must not move underneath the
    /// draw.
    public func books(_ query: Query = Query()) throws -> [Book] {
        try withOptionalCString(query.search) { search in
            try withOptionalCString(query.series) { series in
                var raw = cb_book_query()
                raw.search = search
                raw.series = series
                raw.collection = query.collection ?? 0
                raw.state = query.state?.code ?? UInt32(CB_STATE_ANY.rawValue)
                raw.sort = query.sort.code
                // Zero is "all of them" at the boundary, which is what
                // makes a default-constructed query the whole shelf.
                raw.limit = query.limit ?? 0
                raw.offset = query.offset

                var shelf: OpaquePointer?
                try check(cb_library_query(self.raw, &raw, &shelf))
                guard let shelf else { throw ChapbookError.openFailure() }
                defer { cb_shelf_free(shelf) }

                var count = 0
                try check(cb_shelf_len(shelf, &count))
                return try (0..<count).map { try Self.read(shelf, $0) }
            }
        }
    }

    private static func read(_ shelf: OpaquePointer, _ index: Int) throws -> Book {
        var raw = cb_book()
        try check(cb_shelf_book(shelf, index, &raw))

        let filePath = readString { cb_shelf_file_path(shelf, index, $0, $1, $2) } ?? ""
        return Book(
            id: raw.id,
            title: readString { cb_shelf_title(shelf, index, $0, $1, $2) } ?? "",
            authors: (0..<raw.author_count).compactMap { author in
                readString { cb_shelf_author(shelf, index, author, $0, $1, $2) }
            },
            series: readString { cb_shelf_series(shelf, index, $0, $1, $2) },
            seriesIndex: raw.has_series_index ? raw.series_index : nil,
            language: readString { cb_shelf_language(shelf, index, $0, $1, $2) },
            identifier: readString { cb_shelf_identifier(shelf, index, $0, $1, $2) },
            fingerprint: readString { cb_shelf_fingerprint(shelf, index, $0, $1, $2) } ?? "",
            fileURL: filePath.isEmpty ? nil : URL(fileURLWithPath: filePath),
            coverURL: raw.has_cover
                ? readString { cb_shelf_cover_path(shelf, index, $0, $1, $2) }
                    .map { URL(fileURLWithPath: $0) }
                : nil,
            collections: (0..<raw.collection_count).compactMap { which in
                var id: Int64 = 0
                guard cb_shelf_collection_id(shelf, index, which, &id) == C.ok,
                    let name = readString({ cb_shelf_collection_name(shelf, index, which, $0, $1, $2) })
                else { return nil }
                return CollectionRef(id: id, name: name)
            },
            state: ReadingState(code: raw.state) ?? .unread,
            progress: raw.has_progress ? raw.progress : nil,
            addedAt: Date(timeIntervalSince1970: TimeInterval(raw.added_at)),
            // 0 means never, not 1970.
            lastRead: raw.last_read == 0
                ? nil : Date(timeIntervalSince1970: TimeInterval(raw.last_read)),
            finishedAt: raw.finished_at == 0
                ? nil : Date(timeIntervalSince1970: TimeInterval(raw.finished_at))
        )
    }

    // MARK: Collections

    /// Every collection, with its size, oldest first.
    public func collections() throws -> [Collection] {
        var needed = 0
        let probe = cb_library_collections(raw, nil, 0, &needed)
        if probe == C.ok && needed == 0 { return [] }
        guard probe == C.bufferTooSmall else { throw ChapbookError.last(probe) }

        var buffer = [cb_collection](repeating: cb_collection(), count: needed)
        try buffer.withUnsafeMutableBufferPointer { slot in
            try check(cb_library_collections(raw, slot.baseAddress, slot.count, &needed))
        }
        return buffer.map { entry in
            Collection(
                id: entry.id,
                name: readString { cb_library_collection_name(self.raw, entry.id, $0, $1, $2) } ?? "",
                addedAt: Date(timeIntervalSince1970: TimeInterval(entry.added_at)),
                bookCount: entry.books)
        }
    }

    /// Make a collection, or return the one that already has this name.
    /// Idempotent, so putting a book on "Sci-Fi" need not ask first.
    @discardableResult
    public func createCollection(named name: String) throws -> Int64 {
        var id: Int64 = 0
        try check(cb_library_create_collection(raw, name, &id))
        return id
    }

    public func renameCollection(_ collection: Int64, to name: String) throws {
        try check(cb_library_rename_collection(raw, collection, name))
    }

    /// The books stay; only the grouping goes, and the name frees up.
    public func deleteCollection(_ collection: Int64) throws {
        try check(cb_library_delete_collection(raw, collection))
    }

    public func add(book: Int64, to collection: Int64) throws {
        try check(cb_library_add_to_collection(raw, book, collection))
    }

    public func remove(book: Int64, from collection: Int64) throws {
        try check(cb_library_remove_from_collection(raw, book, collection))
    }

    // MARK: Books

    /// Take a book off the shelf. Soft: the row keeps its id, its
    /// position and its annotations, so opening the same file again is
    /// the same book with its marks intact.
    public func delete(book: Int64) throws {
        try check(cb_library_delete_book(raw, book))
    }

    /// Put a file on the shelf, answering with the row it became.
    ///
    /// **Where a download the app ran itself comes back.** Take a
    /// `Catalog.DownloadRequest`, fetch it with a background
    /// `URLSession`, and hand the finished file here. The format is read
    /// from the bytes, so the name does not matter — which is just as
    /// well, because `URLSession` lands a download in a temp file under a
    /// name of its own.
    ///
    /// The file is not consumed: the library copies what it imports and
    /// never deletes the source, which belongs to whoever passed it —
    /// unlike `Catalog.download(_:into:)`, which removes the staging file
    /// it made itself. Importing the same bytes twice answers with the
    /// same row rather than shelving a duplicate, so a transfer the
    /// system restarted, or a completion delivered twice, needs no
    /// coordination with this call.
    ///
    /// Sync services are not in the file. They live in the catalog entry,
    /// so pass the `progressionURL` and `annotationContainer` captured in
    /// the `DownloadRequest` — *before* the transfer, while the feed was
    /// open — to `setSyncTargets` once this returns a row.
    ///
    /// **Blocking**: it copies a whole book. Keep it off the main actor.
    public func importFile(at url: URL) throws -> Int64 {
        var book: Int64 = 0
        try check(cb_library_import_file(raw, url.path, &book))
        return book
    }

    /// Mark a book finished, or take the mark back.
    ///
    /// A session records this itself when the reader reaches the end, so
    /// this is the other direction — the "mark as read" a reader taps for
    /// a book they finished elsewhere. Marking twice keeps the first
    /// timestamp.
    public func setFinished(_ finished: Bool, book: Int64) throws {
        try check(cb_library_set_finished(raw, book, finished))
    }
}

extension Session {
    /// Drop this book's own settings override — scalar settings and the
    /// chosen typeface both — so it follows the reader's default again,
    /// applying that default now and keeping the place. The undo for a
    /// `setSettings(_:scope:)` with `.thisBook`; a no-op for a book that
    /// never reached the library.
    public func clearBookSettings() throws {
        try check(cb_session_clear_book_settings(raw))
    }

    /// The library row this session's book was imported into, or `nil`
    /// for a book that never reached one — an OPDS stream, or a session
    /// configured without a library directory.
    ///
    /// The join between the reading view and the shelf: opening a book is
    /// what adds it, so this is how an app learns which `Library.Book` it
    /// just created.
    public func bookID() throws -> Int64? {
        var id: Int64 = 0
        let status = cb_session_book_id(raw, &id)
        if status == C.unavailable { return nil }
        try check(status)
        return id
    }
}

/// `withCString` that tolerates `nil`, which is how every optional
/// narrowing field crosses.
func withOptionalCString<R>(
    _ value: String?, _ body: (UnsafePointer<CChar>?) throws -> R
) rethrows -> R {
    guard let value else { return try body(nil) }
    return try value.withCString(body)
}
