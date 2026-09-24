import Chapbook
import Foundation
import Testing

@testable import ChapbookAppModel

// The reader chrome's flows, driven the way the sheets drive them
// through the model: a book opened off the main actor and handed over; a
// search walked unit by unit and a hit selected; a word under a point
// becoming a highlight the marks list shows, recolours and removes; a
// setting scoped to one book and forgotten again; the place saved when
// the screen closes.

@MainActor
private func reader(_ name: String) async throws -> (ReaderViewModel, Reading, URL) {
    let (app, dir) = try container(name)
    guard case .book(let id) = await app.opener.add(try handed(dir, "book.epub")) else {
        throw TestFailure("not added")
    }
    let vm = ReaderViewModel(bookID: id, shelf: app.shelf, opener: app.opener, preferences: app.preferences)
    await settle { if case .opening = vm.state { return false } else { return true } }
    guard case .reading(let reading) = vm.state else {
        throw TestFailure("did not open")
    }
    // Metrics and one frame: nothing about a page is answerable before
    // it is laid out.
    try reading.session.setMetrics(PageMetrics(width: 360, height: 640, dpiScale: 1))
    _ = try reading.session.renderImage()
    vm.moved(try reading.session.position())
    return (vm, reading, dir)
}

@Test @MainActor func aBookOpensIntoTheModelWithItsContentsAndPlace() async throws {
    let (vm, reading, dir) = try await reader("open")
    defer { try? FileManager.default.removeItem(at: dir) }
    #expect(reading.kind == .epub)
    #expect(!reading.contents.isEmpty)
    #expect(reading.fontFamilies.contains("Crimson Text"))
    #expect(vm.place.title == reading.session.title())
    #expect(vm.place.spineLength > 0 && vm.place.pageCount > 0)
    #expect(vm.settings != nil)
    #expect(vm.marks.isEmpty)
    #expect((try reading.session.cacheBudget()) == ReaderViewModel.memoryBudget())
}

@Test @MainActor func theWholeBookBarMovesWithTheReaderAndTheReadoutIsAPreference() async throws {
    let (vm, reading, dir) = try await reader("progress")
    defer { try? FileManager.default.removeItem(at: dir) }
    let s = reading.session

    // Spine-weighted: the first page of the first unit is the start,
    // and a unit further on is a `1/spineLength` slice further along.
    #expect(vm.place.bookFraction == 0)
    let units = try s.spineLength()
    _ = try s.nextUnit()
    vm.moved(try s.position())
    #expect(abs(vm.place.bookFraction - 1 / Double(units)) < 0.001)
    #expect(vm.place.bookFraction <= 1)

    // The readout's words are the shell's preference, kept in defaults.
    #expect(vm.preferences.progressLabel == .percent)
    vm.preferences.setProgressLabel(.pagesLeft)
    #expect(vm.preferences.progressLabel == .pagesLeft)
    let again = Preferences(defaults: UserDefaults(suiteName: "chapbook-app-test-progress-\(ProcessInfo.processInfo.processIdentifier)")!)
    #expect(again.progressLabel == .pagesLeft)
}

@Test @MainActor func aSearchWalksTheBookUnitByUnitAndAHitCanBeShown() async throws {
    let (vm, reading, dir) = try await reader("search")
    defer { try? FileManager.default.removeItem(at: dir) }
    let s = reading.session

    // A word the page actually shows, so the hit is not a guess.
    let page = try #require(try s.speakablePage())
    let word = try #require(
        page.text.split(whereSeparator: \.isWhitespace)
            .map { $0.trimmingCharacters(in: .punctuationCharacters) }
            .first { $0.count >= 4 })
    vm.search(word)
    await settle { !vm.search.running }
    #expect(!vm.search.hits.isEmpty, "found \(word)")
    let hit = try #require(vm.search.hits.first)
    #expect(hit.context.localizedCaseInsensitiveContains(word))

    // Going to a hit and selecting it is what the results list does.
    var redrawn = 0
    vm.onNeedsRedraw = { redrawn += 1 }
    vm.go(toHit: hit)
    #expect(redrawn == 1)
    #expect(try s.selectedRange() == hit.locators)
    #expect(try s.selectedText()?.trimmingCharacters(in: .whitespaces).lowercased() == word.lowercased())
    #expect(try !s.rects(for: hit.locators).isEmpty, "the selection has geometry")

    vm.clearSearch()
    #expect(vm.search.hits.isEmpty)
}

@Test @MainActor func aWordUnderTheFingerBecomesAHighlightTheMarksListKeeps() async throws {
    let (vm, reading, dir) = try await reader("marks")
    defer { try? FileManager.default.removeItem(at: dir) }
    let s = reading.session

    let run = try #require(try s.pageTextRuns()?.first { !$0.text.trimmingCharacters(in: .whitespaces).isEmpty })
    let point = CGPoint(x: run.rect.minX + 4, y: run.rect.midY)
    #expect(try s.selectWord(at: point), "a word is there")
    vm.selected(try s.selectedRange())
    let quote = try #require(vm.selection?.text)
    #expect(!quote.isEmpty)

    vm.highlightSelection()
    #expect(vm.selection == nil)
    #expect(try s.selectedRange() == nil)
    #expect(vm.marks.map(\.kind) == [.highlight])
    let id = try #require(vm.marks.first?.id)
    #expect(vm.marks.first?.text?.trimmingCharacters(in: .whitespaces) == quote.trimmingCharacters(in: .whitespaces))

    // The tap's second question: is a stored highlight under the finger?
    #expect(try s.highlight(at: point) == id)
    vm.recolorHighlight(id, color: "#ffe082")
    #expect(vm.marks.first?.color == "#ffe082")

    vm.addBookmark()
    #expect(vm.marks.count == 2)
    vm.removeMark(id)
    #expect(vm.marks.map(\.kind) == [.bookmark])
    #expect(try s.highlight(at: point) == nil)
}

@Test @MainActor func aSettingScopedToThisBookIsForgottenOnReset() async throws {
    let (vm, _, dir) = try await reader("settings")
    defer { try? FileManager.default.removeItem(at: dir) }
    let before = try #require(vm.settings)
    var bigger = before
    bigger.baseFontSize += 6
    vm.apply(bigger, thisBook: true)
    #expect(vm.settings?.baseFontSize == before.baseFontSize + 6)
    vm.setFontFamily("Crimson Text", thisBook: true)
    #expect(vm.fontFamily == "Crimson Text")
    vm.resetBookSettings()
    #expect(vm.settings?.baseFontSize == before.baseFontSize)
    #expect(vm.fontFamily == nil)
}

@Test @MainActor func closingTheScreenSavesThePlaceForTheShelf() async throws {
    let (vm, reading, dir) = try await reader("close")
    defer { try? FileManager.default.removeItem(at: dir) }
    let shelf = Shelf(directory: dir)
    #expect(try await shelf.book(reading.book.id)?.state == .unread)
    _ = try reading.session.nextPage()
    vm.close()
    guard case .gone = vm.state else {
        Issue.record("still open")
        return
    }
    // The shelf sees the book as being read, with its progress, without
    // the app having gone to the background.
    let row = try #require(try await shelf.book(reading.book.id))
    #expect(row.state == .reading)
    #expect(row.lastRead != nil)
}
