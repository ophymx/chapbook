import Chapbook
import Foundation
import Testing

@testable import ChapbookAppModel

// A file comes in through the opener, shows up on the shelf, opens for
// reading, and is found again.

@Test @MainActor func aHandedFileIsCopiedInAndShelvedAndOpens() async throws {
    let (app, dir) = try container("copy")
    defer { try? FileManager.default.removeItem(at: dir) }

    // A file in the app's own container is the case a share presents:
    // the bytes are ours now, so the opener copies and takes the inbox
    // file away.
    let file = try handed(dir, "handed.epub")
    guard case .book(let id) = await app.opener.add(file) else {
        Issue.record("not added")
        return
    }
    let books = try await app.shelf.books()
    #expect(books.count == 1)
    #expect(books[0].id == id)
    #expect(books[0].fileURL != nil, "the library holds its own copy")
    #expect(books[0].state == .unread)
    #expect(!FileManager.default.fileExists(atPath: file.path), "the inbox is cleaned")

    // The copy is what opens.
    let session = try app.opener.open(books[0])
    #expect(session != nil)
}

@Test @MainActor func theSameBytesTwiceAreOneRow() async throws {
    let (app, dir) = try container("twice")
    defer { try? FileManager.default.removeItem(at: dir) }

    guard case .book(let first) = await app.opener.add(try handed(dir, "a")),
        case .book(let second) = await app.opener.add(try handed(dir, "b.epub"))
    else {
        Issue.record("not added")
        return
    }
    #expect(first == second)
    #expect(try await app.shelf.books().count == 1)
}

@Test @MainActor func junkIsRefusedAndTheShelfStaysClean() async throws {
    let (app, dir) = try container("junk")
    defer { try? FileManager.default.removeItem(at: dir) }

    let junk = dir.appendingPathComponent("notes.txt")
    try Data("not a book".utf8).write(to: junk)
    guard case .failed = await app.opener.add(junk) else {
        Issue.record("junk was shelved")
        return
    }
    #expect(try await app.shelf.books().isEmpty)
}

@Test @MainActor func anAdoptedBookKeepsNoCopyAndIsFoundAgainByItsGrant() async throws {
    let (app, dir) = try container("adopt")
    defer { try? FileManager.default.removeItem(at: dir) }

    // Adoption is the picker's door: a bookmark, a record by content, no
    // copy. A file outside the container would arrive with a security
    // scope; the bookmark dance is the same for one inside it.
    let file = try handed(dir, "picked.epub")
    guard case .book(let id) = await app.opener.adopt(file) else {
        Issue.record("not adopted")
        return
    }
    let book = try #require(try await app.shelf.book(id))
    #expect(book.fileURL == nil, "the platform owns the file")
    #expect(app.grants.bookmark(for: book.fingerprint) != nil)
    #expect(FileManager.default.fileExists(atPath: file.path))

    // Opening resolves the grant; a session over a descriptor.
    let session = try #require(try app.opener.open(book))
    #expect(session.title()?.isEmpty == false)
}

@Test @MainActor func aBookWhoseFileIsGoneDoesNotOpen() async throws {
    let (app, dir) = try container("gone")
    defer { try? FileManager.default.removeItem(at: dir) }

    // Adopted, then the grant is lost: the shelf row survives and the
    // reader is told the file is out of reach rather than crashed.
    let file = try handed(dir, "adopted.epub")
    guard case .book(let id) = await app.opener.adopt(file) else {
        Issue.record("not adopted")
        return
    }
    let book = try #require(try await app.shelf.book(id))
    let grant = try #require(app.grants.bookmark(for: book.fingerprint))
    app.grants.forget(book.fingerprint)
    #expect(try app.opener.open(book) == nil)
    app.grants.remember(Data("not a bookmark".utf8), for: book.fingerprint)
    #expect(try app.opener.open(book) == nil)
    // The grant is good and the file is gone from under it.
    app.grants.remember(grant, for: book.fingerprint)
    try FileManager.default.removeItem(at: file)
    #expect(try app.opener.open(book) == nil)
}

@Test @MainActor func aGrantIsRememberedByFingerprintAndForgotten() throws {
    let (app, dir) = try container("grants")
    defer { try? FileManager.default.removeItem(at: dir) }

    let fingerprint = "test-\(UUID().uuidString)"
    let bookmark = Data("bookmark".utf8)
    #expect(app.grants.bookmark(for: fingerprint) == nil)
    app.grants.remember(bookmark, for: fingerprint)
    #expect(app.grants.bookmark(for: fingerprint) == bookmark)
    app.grants.forget(fingerprint)
    #expect(app.grants.bookmark(for: fingerprint) == nil)
}

@Test @MainActor func theShelfModelListsSortsFiltersAndMarks() async throws {
    let (app, dir) = try container("shelf-vm")
    defer { try? FileManager.default.removeItem(at: dir) }

    guard case .book(let id) = await app.opener.add(try handed(dir, "one.epub")) else {
        Issue.record("not added")
        return
    }
    let vm = ShelfViewModel(shelf: app.shelf, opener: app.opener, downloads: app.downloads)
    await settle { !vm.state.loading }
    #expect(vm.state.books.map(\.id) == [id])
    #expect(vm.state.sort == .read, "a shelf opens on the book the reader was in")

    vm.setStateFilter(.finished)
    await settle { vm.state.books.isEmpty }
    #expect(vm.state.books.isEmpty)

    let row = try #require(try await app.shelf.book(id))
    vm.setFinished(row, finished: true)
    await settle { vm.state.books.count == 1 }
    #expect(vm.state.books.first?.state == .finished)

    vm.setStateFilter(nil)
    vm.setSearch("zzz-no-such-book")
    await settle { vm.state.books.isEmpty }
    #expect(vm.state.books.isEmpty)
}
