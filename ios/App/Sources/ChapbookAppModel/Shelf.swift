import Chapbook
import Foundation

/// The library and the application, on the one thread they are allowed
/// to be on.
///
/// A `Library` is a SQLite connection and an `App` holds one: movable,
/// never shared. This actor runs on a serial queue of its own, so every
/// call hops there and back and a view model can ask from the main actor
/// without knowing that. Both open on first use, on that queue, and stay
/// open for the life of the process — the database is WAL, so a reading
/// session holding its own connection beside these is the ordinary
/// arrangement.
public actor Shelf {
    private let platform: Platform
    private let queue = DispatchSerialQueue(label: "chapbook-shelf")
    private var library: Library?
    private var app: App?

    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    public init(platform: Platform) {
        self.platform = platform
    }

    private func open() throws -> Library {
        if let library { return library }
        let opened = try Library(directory: platform.libraryDirectory)
        library = opened
        return opened
    }

    private func openApp() throws -> App {
        if let app { return app }
        let opened = try platform.open()
        app = opened
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
        try openApp().importFile(at: url)
    }

    /// What a landed download does: shelve the file and record where the
    /// book syncs — the two services off the catalog entry it came from,
    /// read before the transfer. The file stays the caller's.
    public func landDownload(at url: URL, progressionURL: URL?, annotationContainer: URL?) throws -> Int64 {
        try openApp().landDownload(
            at: url, progressionURL: progressionURL, annotationContainer: annotationContainer)
    }

    /// Record where a book syncs — the two services off the catalog entry
    /// it came from — for a book that arrived some other way.
    public func setSyncTargets(book: Int64, progressionURL: URL?, annotationContainer: URL?) throws {
        try open().setSyncTargets(
            book: book, progressionURL: progressionURL, annotationContainer: annotationContainer)
    }

    public func syncProgressionURL(book: Int64) throws -> URL? {
        try open().syncProgressionURL(book: book)
    }

    // MARK: Grants, kept by the engine beside the shelf

    public func grant(for fingerprint: String) throws -> Data? {
        try openApp().grant(for: fingerprint)
    }

    public func remember(grant: Data, for fingerprint: String) throws {
        try openApp().remember(grant: grant, for: fingerprint)
    }

    public func forgetGrant(for fingerprint: String) throws {
        try openApp().forgetGrant(for: fingerprint)
    }
}
