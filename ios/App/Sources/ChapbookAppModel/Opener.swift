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
/// is ours to clean and the bytes are already ours. Which door a URL
/// takes is decided here, because only the platform knows what its
/// grants are worth; what each door does is the engine's, and the same
/// on every platform.
public final class Opener: Sendable {
    private let platform: Platform
    private let shelf: Shelf
    /// The app's own container, which decides which door a URL takes.
    private let container: String

    /// `container` is the app's own sandbox by default; a test names its
    /// scratch directory, since a temp dir on a Mac is not under home.
    public init(platform: Platform, shelf: Shelf, container: URL? = nil) {
        self.platform = platform
        self.shelf = shelf
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

    /// Where the bundled faces are: `fonts/` beside the executable, the
    /// same place the demo keeps them. A test without a bundle points
    /// `fontsDirectory` at the fixtures.
    nonisolated(unsafe) public static var fontsDirectory: URL =
        URL(fileURLWithPath: Bundle.main.resourcePath ?? ".").appendingPathComponent("fonts")

    static var fonts: FontSource {
        .embedded(directory: fontsDirectory, family: "Crimson Text")
    }

    /// Adopt: bookmark the grant, then let the engine open the file once
    /// to record the book and keep the bookmark under the fingerprint
    /// that opening produced.
    public func adopt(_ url: URL) async -> Added {
        let bookmark: Data
        do {
            bookmark = try SecurityScopedBook.bookmark(for: url)
        } catch {
            return .failed("no bookmark for \(url.lastPathComponent): \(error)")
        }
        let platform = self.platform
        // An `App` of this call's own: one library connection, opened,
        // used on this thread and dropped, so nothing here shares a
        // handle with the shelf's actor.
        let adopted: Result<Int64, Error> = await offMain {
            let resolved = try SecurityScopedBook.open(bookmark)
            return try platform.open().adopt(fileDescriptor: resolved.fileDescriptor, grant: bookmark)
        }
        switch adopted {
        case .failure(let error):
            return .failed("\(url.lastPathComponent) is not a book: \(error)")
        case .success(let id):
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
    /// Blocking; the caller keeps it off the main actor. Opens an `App`
    /// of its own for the call, as `adopt(_:)` does.
    public func open(_ book: Library.Book, cacheBudget: Int? = nil) throws -> Session? {
        let app = try platform.open()
        switch try app.open(book: book.id) {
        case .session(let session):
            if let budget = cacheBudget { try session.setCacheBudget(budget) }
            return session
        case .missing:
            return nil
        case .adopted:
            // The platform's half of custody: the remembered bookmark,
            // resolved to a descriptor, opened with the app's own
            // configuration.
            guard let bookmark = try app.grant(for: book.fingerprint) else { return nil }
            guard let resolved = try? SecurityScopedBook.open(bookmark) else { return nil }
            return try app.open(.fileDescriptor(resolved.fileDescriptor), cacheBudgetBytes: cacheBudget)
        }
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
