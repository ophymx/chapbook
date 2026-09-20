import Foundation
import Testing

@testable import Chapbook

// The shelf, driven the way an app draws one: open books to put them
// there, then browse. The macOS slice runs these with no simulator; the
// boundary is byte-for-byte the one the iOS slices carry.

private let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // ChapbookTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // Chapbook
    .deletingLastPathComponent()  // ios
    .deletingLastPathComponent()  // repo root
    .appendingPathComponent("fixtures")

private func fonts() -> FontSource {
    .embedded(directory: fixtures.appendingPathComponent("fonts"), family: "Crimson Text")
}

private func scratch(_ name: String) throws -> URL {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent(
            "chapbook-swift-shelf-\(ProcessInfo.processInfo.processIdentifier)-\(name)")
    try? FileManager.default.removeItem(at: dir)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

/// A book reaches the library by being opened, so stocking the shelf is
/// three reads. Returns the directory they landed in.
private func stocked(_ name: String) throws -> URL {
    let dir = try scratch(name)
    for book in ["epub/minimal.epub", "epub/series.epub", "epub/series-legacy.epub"] {
        _ = try Session(
            source: .path(fixtures.appendingPathComponent(book)),
            configuration: SessionConfiguration(fonts: fonts(), libraryDirectory: dir))
    }
    return dir
}

@Test func theShelfListsWhatWasOpened() throws {
    let dir = try stocked("list")
    defer { try? FileManager.default.removeItem(at: dir) }

    let library = try Library(directory: dir)
    let books = try library.books()
    #expect(books.count == 3)
    // A default query is the whole shelf, newest first.
    #expect(books.allSatisfy { $0.state == .unread })
    #expect(books.allSatisfy { !$0.fingerprint.isEmpty })
    #expect(books.allSatisfy { $0.fileURL != nil })
}

@Test func aSearchFoldsCaseAndAccentsAndReachesTheSeries() throws {
    let dir = try stocked("search")
    defer { try? FileManager.default.removeItem(at: dir) }
    let library = try Library(directory: dir)

    let cycle = try library.books(Library.Query(search: "fixture cycle", sort: .series))
    #expect(cycle.count == 2)
    #expect(cycle.first?.series == "The Fixture Cycle")
    // Sorted by position within the series, and the position is
    // fractional because `group-position` is.
    #expect(cycle.first?.seriesIndex == 1)
    #expect(cycle.last?.seriesIndex == 2.5)

    // A book in no series says so with nil rather than an empty string.
    let minimal = try library.books(Library.Query(search: "minimal"))
    #expect(minimal.count == 1)
    #expect(minimal.first?.series == nil)
    #expect(minimal.first?.seriesIndex == nil)
    #expect(minimal.first?.language == "en")

    // A reader typing a real title is not composing a query language.
    #expect(try library.books(Library.Query(search: "Legacy-Fixture")).count == 1)
    #expect(try library.books(Library.Query(search: "!!!")).isEmpty)
}

@Test func collectionsGroupBooksAndShowUpOnTheRows() throws {
    let dir = try stocked("collections")
    defer { try? FileManager.default.removeItem(at: dir) }
    let library = try Library(directory: dir)

    #expect(try library.collections().isEmpty)
    let shelf = try library.createCollection(named: "To Reread")
    // Idempotent on the name.
    #expect(try library.createCollection(named: "To Reread") == shelf)

    let first = try #require(try library.books().first)
    try library.add(book: first.id, to: shelf)

    let listed = try library.collections()
    #expect(listed.count == 1)
    #expect(listed.first?.name == "To Reread")
    #expect(listed.first?.bookCount == 1)

    let members = try library.books(Library.Query(collection: shelf))
    #expect(members.count == 1)
    #expect(members.first?.collections.map(\.name) == ["To Reread"])

    // Deleting takes the grouping, not the books.
    try library.deleteCollection(shelf)
    #expect(try library.collections().isEmpty)
    #expect(try library.books().count == 3)
}

@Test func finishingIsRecordedAndIsNotProgress() throws {
    let dir = try scratch("finish")
    defer { try? FileManager.default.removeItem(at: dir) }

    let session = try Session(
        source: .path(fixtures.appendingPathComponent("epub/minimal.epub")),
        configuration: SessionConfiguration(fonts: fonts(), libraryDirectory: dir))
    let id = try #require(try session.bookID())

    let library = try Library(directory: dir)
    try library.setFinished(true, book: id)

    let finished = try library.books(Library.Query(state: .finished))
    #expect(finished.count == 1)
    #expect(finished.first?.id == id)
    #expect(finished.first?.finishedAt != nil)
    // Never opened far enough to have one, and finished all the same:
    // the two are different questions.
    #expect(finished.first?.progress == nil)

    try library.setFinished(false, book: id)
    #expect(try library.books(Library.Query(state: .finished)).isEmpty)

    // Soft removal keeps the row, so the shelf empties without the
    // book's history going with it.
    try library.delete(book: id)
    #expect(try library.books().isEmpty)
}

// MARK: A download the app ran itself, coming back

// `importFile(at:)` is the completing half of a transfer that ran under
// the platform's own job system rather than inside a blocking call. The
// properties below are the ones a background download leans on, and this
// is the only place they are exercised from Swift.

/// Stage a copy of a fixture under `name`, as a finished transfer would
/// have left it. Returns where it landed.
private func handedOver(_ fixture: String, as name: String, in dir: URL) throws -> URL {
    let destination = dir.appendingPathComponent(name)
    try FileManager.default.copyItem(
        at: fixtures.appendingPathComponent(fixture), to: destination)
    return destination
}

@Test func aFileTheAppFetchedItselfIsShelvedWithoutBeingConsumed() throws {
    let dir = try scratch("import")
    defer { try? FileManager.default.removeItem(at: dir) }
    let library = try Library(directory: dir)

    // The name a background `URLSession` actually produces: a temp file
    // under an opaque name, no extension anywhere. The format is read
    // from the bytes, so it shelves regardless.
    let source = try handedOver(
        "epub/minimal.epub", as: "CFNetworkDownload_a8Kq2p", in: dir)
    let book = try library.importFile(at: source)
    #expect(book > 0, "the import answers with its library row")

    let shelved = try library.books()
    #expect(shelved.count == 1)
    #expect(shelved.first?.id == book)
    #expect(shelved.first?.fingerprint.isEmpty == false)

    // The source belongs to whoever passed it — a `URLSession` temp file,
    // a document-browser pick — so the library copies and never reaches
    // into the host's storage to clean up. `Catalog.download(_:into:)`
    // removes its staging file because it made that file itself; this is
    // the opposite case, and the difference is the whole distinction.
    #expect(FileManager.default.fileExists(atPath: source.path))
    #expect(shelved.first?.fileURL?.path != source.path)
}

@Test func importingTheSameBytesTwiceAnswersWithTheSameRow() throws {
    let dir = try scratch("import-retry")
    defer { try? FileManager.default.removeItem(at: dir) }
    let library = try Library(directory: dir)

    // A different path and a different name, because a job system that
    // retries rarely lands the bytes in the same place twice.
    let first = try handedOver("epub/minimal.epub", as: "attempt-1.epub", in: dir)
    let second = try handedOver("epub/minimal.epub", as: "attempt-2", in: dir)

    let book = try library.importFile(at: first)
    let again = try library.importFile(at: second)

    // Books are identified by a fingerprint of their bytes, which is what
    // lets a `URLSession` completion delivered twice, or a worker the
    // system restarted, stay correct without coordinating with the shelf.
    #expect(book == again, "the same bytes are the same book")
    #expect(try library.books().count == 1, "no duplicate row")
}

@Test func aFinishedDownloadIsShelvedThenGivenTheServicesItsFeedCarried() throws {
    let dir = try scratch("import-sync")
    defer { try? FileManager.default.removeItem(at: dir) }
    let library = try Library(directory: dir)

    // The documented completion, in order: the file first, then the two
    // services — which came from the catalog entry, captured before the
    // transfer, because the feed is usually gone by the time it lands.
    let source = try handedOver("epub/minimal.epub", as: "landed", in: dir)
    let book = try library.importFile(at: source)
    let progression = URL(string: "http://catalog.test/sync/position/v3")!
    let container = URL(string: "http://catalog.test/sync/annotations/v3")!
    try library.setSyncTargets(
        book: book, progressionURL: progression, annotationContainer: container)

    // Sync services are not in the file, so nothing but this call could
    // have put them there. A book that skipped it is one that will never
    // reconcile, which is the failure this ordering exists to prevent.
    #expect(library.syncProgressionURL(book: book) == progression)
    #expect(library.syncAnnotationContainer(book: book) == container)
}

@Test func importingSomethingThatIsNotABookFailsInsteadOfCrashing() throws {
    let dir = try scratch("import-junk")
    defer { try? FileManager.default.removeItem(at: dir) }
    let library = try Library(directory: dir)

    // A truncated transfer is an ordinary outcome for a background job,
    // and the host has to be able to tell the difference between "retry"
    // and a crash.
    let junk = dir.appendingPathComponent("truncated.epub")
    try Data("not a book".utf8).write(to: junk)

    #expect(throws: ChapbookError.self) {
        try library.importFile(at: junk)
    }
    #expect(try library.books().isEmpty, "nothing half-shelved")

    // A path with no file behind it is the other half of the same
    // question — a completion handler handed a URL the system already
    // cleaned up.
    #expect(throws: ChapbookError.self) {
        try library.importFile(at: dir.appendingPathComponent("never-existed.epub"))
    }
}
