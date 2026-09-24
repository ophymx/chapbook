import Chapbook
import Foundation

/// What adding a file came to.
public enum Added: Sendable {
    /// On the shelf, under this row.
    case book(Int64)
    /// Not a book, or not reachable. `reason` is for the log, not the
    /// reader.
    case failed(String)
}

/// The two doors a book comes in through, and the one it goes out to.
///
/// **Custody follows the grant.** A URL from the document picker or from
/// Files' "open in place" carries a security scope the app can bookmark,
/// so it is *adopted*: the library records the book by content and keeps
/// no copy, because the file is the platform's and a copy would be a
/// second one to keep in step. A file that landed in the app's own
/// container — the `Inbox` a "Copy to Chapbook" share fills — is
/// *imported*, a copy made and the inbox file removed, because the inbox
/// is ours to clean and the bytes are already ours. Both land on the same
/// shelf; only `Library.Book.fileURL` tells them apart.
public final class Opener: Sendable {
    private let libraryDirectory: URL
    private let shelf: Shelf
    private let grants: Grants
    /// The app's own container, which decides which door a URL takes.
    private let container: String

    /// `container` is the app's own sandbox by default; a test names its
    /// scratch directory, since a temp dir on a Mac is not under home.
    public init(libraryDirectory: URL, shelf: Shelf, grants: Grants, container: URL? = nil) {
        self.libraryDirectory = libraryDirectory
        self.shelf = shelf
        self.grants = grants
        self.container = (container?.standardizedFileURL.path ?? NSHomeDirectory())
    }

    /// A picked, shared or handed-over file. The door is chosen by where
    /// the file is; both are blocking and run off the caller's actor.
    public func add(_ url: URL) async -> Added {
        if url.standardizedFileURL.path.hasPrefix(container) {
            return await importCopy(of: url, removingSource: true)
        }
        return await adopt(url)
    }

    /// The configuration every session in this app opens with: the
    /// bundled faces, the app's library, and the platform's transport.
    public func configuration(cacheBudget: Int? = nil) -> SessionConfiguration {
        SessionConfiguration(
            fonts: Self.fonts, libraryDirectory: libraryDirectory, cacheBudgetBytes: cacheBudget)
    }

    /// Where the bundled faces are: `fonts/` beside the executable, the
    /// same place the demo keeps them. A test without a bundle points
    /// `fontsDirectory` at the fixtures.
    nonisolated(unsafe) public static var fontsDirectory: URL =
        URL(fileURLWithPath: Bundle.main.resourcePath ?? ".").appendingPathComponent("fonts")

    static var fonts: FontSource {
        .embedded(directory: fontsDirectory, family: "Crimson Text")
    }

    /// Adopt: bookmark the grant, open once to record the book, keep the
    /// bookmark under the fingerprint that opening produced.
    public func adopt(_ url: URL) async -> Added {
        let bookmark: Data
        do {
            bookmark = try SecurityScopedBook.bookmark(for: url)
        } catch {
            return .failed("no bookmark for \(url.lastPathComponent): \(error)")
        }
        let configuration = configuration()
        // Opening is what records the book; the session itself is not
        // wanted yet. Nothing is saved on close, so this leaves no mark.
        let opened: Result<Int64?, Error> = await offMain {
            let resolved = try SecurityScopedBook.open(bookmark)
            let session = try Session(
                source: .fileDescriptor(resolved.fileDescriptor), configuration: configuration)
            return try session.bookID()
        }
        switch opened {
        case .failure(let error):
            return .failed("\(url.lastPathComponent) is not a book: \(error)")
        case .success(nil):
            return .failed("\(url.lastPathComponent) did not reach the shelf")
        case .success(.some(let id)):
            if let book = try? await shelf.book(id) {
                grants.remember(bookmark, for: book.fingerprint)
            }
            return .book(id)
        }
    }

    /// Import: copy the bytes into the library, and take the source away
    /// when it was ours to take.
    public func importCopy(of url: URL, removingSource: Bool) async -> Added {
        defer { if removingSource { try? FileManager.default.removeItem(at: url) } }
        do {
            return .book(try await shelf.importFile(at: url))
        } catch {
            return .failed("\(url.lastPathComponent) is not a book: \(error)")
        }
    }

    /// Open a shelf row for reading, or `nil` when its file is out of
    /// reach: a grant the platform revoked, a copy the reader deleted.
    /// Blocking; the caller keeps it off the main actor.
    public func open(_ book: Library.Book, cacheBudget: Int? = nil) throws -> Session? {
        let configuration = configuration(cacheBudget: cacheBudget)
        if let file = book.fileURL {
            guard FileManager.default.fileExists(atPath: file.path) else { return nil }
            return try Session(source: .path(file), configuration: configuration)
        }
        guard let bookmark = grants.bookmark(for: book.fingerprint) else { return nil }
        guard let resolved = try? SecurityScopedBook.open(bookmark) else { return nil }
        return try Session(source: .fileDescriptor(resolved.fileDescriptor), configuration: configuration)
    }
}

/// Run blocking engine work on a background queue and hand the result
/// back — the shape every open in this app takes, because a session is
/// constructed off the main actor and then belongs to it.
public func offMain<T>(_ body: @Sendable @escaping () throws -> sending T) async -> sending Result<T, Error> {
    await withCheckedContinuation { continuation in
        DispatchQueue.global(qos: .userInitiated).async {
            continuation.resume(returning: Result { try body() })
        }
    }
}
