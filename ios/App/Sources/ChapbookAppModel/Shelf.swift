import Chapbook
import Foundation

/// The library, on the one thread it is allowed to be on.
///
/// A `Library` is a SQLite connection: movable, never shared. This actor
/// runs on a serial queue of its own, so every call hops there and back
/// and a view model can ask from the main actor without knowing that.
/// The connection opens on first use, on that queue, and stays open for
/// the life of the process — the database is WAL, so a reading session
/// holding its own connection beside this one is the ordinary
/// arrangement.
public actor Shelf {
    private let directory: URL
    private let queue = DispatchSerialQueue(label: "chapbook-shelf")
    private var library: Library?

    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    public init(directory: URL) {
        self.directory = directory
    }

    private func open() throws -> Library {
        if let library { return library }
        let opened = try Library(directory: directory)
        library = opened
        return opened
    }

    public func books(_ query: Library.Query = Library.Query()) throws -> [Library.Book] {
        try open().books(query)
    }

    /// One row by id, or `nil` once it is gone. The shelf is small enough
    /// to scan.
    public func book(_ id: Int64) throws -> Library.Book? {
        try open().books().first { $0.id == id }
    }

    public func setFinished(_ finished: Bool, book: Int64) throws {
        try open().setFinished(finished, book: book)
    }

    public func remove(book: Int64) throws {
        try open().delete(book: book)
    }

    /// Copy a file into the library. Blocking by nature, which is why it
    /// lives here rather than on the caller's thread.
    public func importFile(at url: URL) throws -> Int64 {
        try open().importFile(at: url)
    }

    /// Record where a book syncs — the two services off the catalog entry
    /// it came from.
    public func setSyncTargets(book: Int64, progressionURL: URL?, annotationContainer: URL?) throws {
        try open().setSyncTargets(
            book: book, progressionURL: progressionURL, annotationContainer: annotationContainer)
    }

    public func syncProgressionURL(book: Int64) throws -> URL? {
        try open().syncProgressionURL(book: book)
    }
}
