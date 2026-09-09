import CoreGraphics
import Foundation
import Testing

@testable import Chapbook

// The reader's interactive surface, driven the way an app drives it:
// contents, jumps, links, selection, marks, search. `minimal.epub` is the
// fixture with all of it — a two-level contents whose third entry carries
// a fragment, and a chapter that links to the next.

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
            "chapbook-swift-reading-\(ProcessInfo.processInfo.processIdentifier)-\(name)")
    try? FileManager.default.removeItem(at: dir)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

private func metrics() -> PageMetrics {
    PageMetrics(
        width: 600, height: 800,
        marginTop: 40, marginRight: 40, marginBottom: 40, marginLeft: 40,
        dpiScale: 1)
}

/// A laid-out session over `minimal.epub`, plus the library it wrote to.
private func reader(_ name: String, book: String = "epub/minimal.epub") throws -> (
    Session, URL
) {
    let dir = try scratch(name)
    let session = try Session(
        source: .path(fixtures.appendingPathComponent(book)),
        configuration: SessionConfiguration(fonts: fonts(), libraryDirectory: dir))
    try session.setMetrics(metrics())
    _ = try session.pageCount()  // forces the layout
    return (session, dir)
}

// MARK: Contents and jumps

@Test func theContentsCarryTheirNestingAndTheirTargets() throws {
    let (session, dir) = try reader("toc")
    defer { try? FileManager.default.removeItem(at: dir) }

    let toc = try session.tableOfContents()
    #expect(toc.count == 3)
    #expect(toc.map(\.label) == [
        "Chapter One: A Beginning",
        "Chapter Two: A Continuation",
        "Part the Second",
    ])
    // The third entry is the nested one, and it points inside its unit.
    #expect(toc.map(\.depth) == [0, 0, 1])
    #expect(toc[2].pointsWithinUnit)
    #expect(!toc[0].pointsWithinUnit)
    #expect(toc.allSatisfy { $0.spine != nil })
    // Indices are the identity a jump takes, so they must be the row's.
    #expect(toc.map(\.id) == [0, 1, 2])

    #expect(try session.go(to: toc[1]))
    #expect(try session.position().spine == 1)
}

@Test func aJumpIsRetractableAndAPageTurnIsNot() throws {
    let (session, dir) = try reader("back")
    defer { try? FileManager.default.removeItem(at: dir) }

    // Nothing jumped yet: nowhere to go back to.
    #expect(try !session.canGoBack())

    // A page turn is not a jump — that is the whole reason `back` is not
    // just "previous page".
    #expect(try session.nextPage())
    #expect(try !session.canGoBack())

    // Neither is a chapter skip. A reader walking through the book is
    // not departing from somewhere they meant to return to, and if this
    // pushed, `back` would spend itself undoing deliberate navigation.
    #expect(try session.prevUnit())
    #expect(try !session.canGoBack(), "a unit skip leaves the trail alone")
    #expect(try session.nextUnit())
    #expect(try !session.canGoBack(), "and so does the other direction")

    let toc = try session.tableOfContents()
    #expect(try session.go(to: toc[1]))
    #expect(try session.canGoBack())
    #expect(try session.apply(.back) == .changed)
    #expect(try !session.canGoBack(), "the trail was one deep")
}

@Test func aLocatorIsAPlaceToComeBackTo() throws {
    let (session, dir) = try reader("locator")
    defer { try? FileManager.default.removeItem(at: dir) }

    try session.nextPage()
    let saved = try session.locator()
    #expect(saved.spine == 1)

    // Save, wander off, come back: the round trip is the property an app
    // actually leans on, and the one that holds exactly.
    try session.go(to: Session.Locator(spine: 0, offset: 0))
    #expect(try session.position().spine == 0)
    #expect(try session.go(to: saved))
    #expect(try session.locator() == saved)
    #expect(try session.position().spine == 1)

    // Make the text bigger. The reader keeps their passage — the offset
    // is where the current page begins, so it may settle on a different
    // boundary within it — and the round trip still holds, which
    // `position()` could not promise: its page number means something
    // else at every font size.
    var settings = try session.settings()
    settings.baseFontSize *= 2
    try session.setSettings(settings, scope: .thisBook)
    _ = try session.pageCount()

    let reflowed = try session.locator()
    #expect(reflowed.spine == saved.spine)
    try session.go(to: Session.Locator(spine: 0, offset: 0))
    #expect(try session.go(to: reflowed))
    #expect(try session.locator() == reflowed)

    // A spine the book does not have is the one honest false.
    #expect(try !session.go(to: Session.Locator(spine: 999, offset: 0)))
}

@Test func anAnchorLandsInTheUnitEvenWhenTheFragmentIsUnknown() throws {
    let (session, dir) = try reader("anchor")
    defer { try? FileManager.default.removeItem(at: dir) }

    #expect(try session.go(toAnchor: "part2", inSpine: 1))
    #expect(try session.position().spine == 1)

    // A fragment the unit does not carry is not a failure: it lands at
    // the unit's start. Only a spine the book lacks answers false.
    #expect(try session.go(toAnchor: "no-such-id", inSpine: 0))
    #expect(try session.position().spine == 0)
    #expect(try !session.go(toAnchor: "part2", inSpine: 999))
}

@Test func unitsSkipAndStopAtTheEnds() throws {
    let (session, dir) = try reader("units")
    defer { try? FileManager.default.removeItem(at: dir) }

    #expect(try !session.prevUnit(), "already in the first unit")
    #expect(try session.nextUnit())
    #expect(try session.position().spine == 1)
    #expect(try !session.nextUnit(), "the book has two units")
    #expect(try session.prevUnit())
    #expect(try session.position().spine == 0)
}

// MARK: Links

@Test func aLinkIsFoundUnderItsOwnWordsAndFollowed() throws {
    let (session, dir) = try reader("links")
    defer { try? FileManager.default.removeItem(at: dir) }

    // Find the link by its text rather than by guessing at a coordinate:
    // the run's rect is where the words actually landed.
    let runs = try #require(try session.pageTextRuns())
    let run = try #require(runs.first { $0.text.contains("the next chapter") })
    let href = try #require(try session.link(at: CGPoint(x: run.rect.midX, y: run.rect.midY)))
    #expect(href.contains("chapter2"))

    // Off the link there is nothing, which is what lets a miss fall
    // through to the tap zones.
    #expect(try session.link(at: CGPoint(x: 5, y: 5)) == nil)

    #expect(try session.follow(link: href))
    #expect(try session.position().spine == 1)
    #expect(try session.canGoBack(), "a followed link pushes the return")

    // An external link is not the engine's to navigate — it answers
    // false and stays put, which is the app's cue to open a browser.
    let stayed = try session.position()
    #expect(try !session.follow(link: "https://example.com/elsewhere"))
    #expect(try session.position() == stayed)
}

// MARK: Selection

@Test func aSelectionIsBuiltDraggedReadAndDropped() throws {
    let (session, dir) = try reader("selection")
    defer { try? FileManager.default.removeItem(at: dir) }

    #expect(try session.selectedRange() == nil)
    #expect(try session.selectedText() == nil)

    let runs = try #require(try session.pageTextRuns())
    let run = try #require(runs.first { $0.text.count > 20 })

    // A long press picks out one word.
    #expect(try session.selectWord(at: CGPoint(x: run.rect.minX + 4, y: run.rect.midY)))
    let word = try #require(try session.selectedRange())
    #expect(!word.isEmpty)
    #expect(try session.selectedText()?.isEmpty == false)

    // A drag to the far end of the line extends it.
    try session.dragSelection(to: CGPoint(x: run.rect.maxX - 2, y: run.rect.midY))
    let dragged = try #require(try session.selectedRange())
    #expect(dragged.upperBound > word.upperBound, "the drag reached further")

    try session.clearSelection()
    #expect(try session.selectedRange() == nil)

    // A press on bare page anchors nothing — the app's cue that the
    // gesture meant something else.
    #expect(try !session.beginSelection(at: CGPoint(x: 2, y: 2)))

    // An exact range is the other way in, which is how a search hit or an
    // adjusted handle becomes the selection.
    try session.select(run.locators)
    #expect(try session.selectedRange() == run.locators)
    #expect(try !session.rects(for: run.locators).isEmpty, "handles have geometry to sit on")
}

// MARK: Annotations

@Test func marksAreMadeFoundRecoloredAndRemoved() throws {
    let (session, dir) = try reader("marks")
    defer { try? FileManager.default.removeItem(at: dir) }

    #expect(try session.annotations().isEmpty)

    let bookmark = try session.addBookmark()
    let runs = try #require(try session.pageTextRuns())
    let run = try #require(runs.first { $0.text.count > 20 })
    try session.select(run.locators)
    let highlight = try session.addHighlight()
    try session.select(run.locators)
    let note = try session.addNote("a thought worth keeping")

    let marks = try session.annotations()
    #expect(marks.count == 3)
    #expect(Set(marks.map(\.id)) == Set([bookmark, highlight, note]))
    #expect(Set(marks.map(\.kind)) == Set([.bookmark, .highlight, .note]))
    // Ordered by progression, and each carries where it resolves.
    #expect(marks.map(\.progression).sorted() == marks.map(\.progression))
    #expect(marks.allSatisfy { $0.spine == 0 })

    let storedNote = try #require(marks.first { $0.kind == .note })
    #expect(storedNote.text == "a thought worth keeping")
    let storedHighlight = try #require(marks.first { $0.kind == .highlight })
    #expect(storedHighlight.text?.isEmpty == false, "a highlight quotes what it covers")
    #expect(storedHighlight.color == nil, "the theme's color until one is chosen")

    // The highlight answers under its own words — after links, before the
    // tap zones.
    let hit = try session.highlight(at: CGPoint(x: run.rect.midX, y: run.rect.midY))
    #expect(hit == highlight)
    #expect(try session.highlight(at: CGPoint(x: 5, y: 5)) == nil)

    try session.setHighlightColor("#ffcc00", for: highlight)
    let recolored = try #require(try session.annotations().first { $0.id == highlight })
    #expect(recolored.color == "#ffcc00")
    // And back to the theme's.
    try session.setHighlightColor(nil, for: highlight)
    #expect(try session.annotations().first { $0.id == highlight }?.color == nil)

    try session.removeAnnotation(highlight)
    #expect(try session.annotations().count == 2)
    #expect(try session.annotations().allSatisfy { $0.id != highlight })
}

@Test func aMarkIsSomewhereToJumpBackTo() throws {
    let (session, dir) = try reader("mark-jump")
    defer { try? FileManager.default.removeItem(at: dir) }

    try session.go(to: Session.Locator(spine: 1, offset: 0))
    let mark = try session.addBookmark()
    try session.go(to: Session.Locator(spine: 0, offset: 0))
    #expect(try session.position().spine == 0)

    #expect(try session.go(toAnnotation: mark))
    #expect(try session.position().spine == 1)
}

@Test func aHighlightNeedsSomethingSelected() throws {
    let (session, dir) = try reader("no-selection")
    defer { try? FileManager.default.removeItem(at: dir) }

    // Reported rather than quietly doing nothing: a shell whose highlight
    // button is live with no selection has a bug worth hearing about, and
    // a silent no-op is exactly what hides it.
    #expect(throws: ChapbookError.self) { try session.addHighlight() }
    #expect(throws: ChapbookError.self) { try session.addNote("nothing to attach to") }
}

// MARK: Search

@Test func searchFindsItsWordsAndPointsAtThem() throws {
    let (session, dir) = try reader("search")
    defer { try? FileManager.default.removeItem(at: dir) }

    let hits = try session.search("chapter")
    #expect(!hits.isEmpty)
    for hit in hits {
        #expect(!hit.locators.isEmpty)
        #expect(!hit.context.isEmpty)
        #expect(hit.matchInContext.upperBound <= UInt32(hit.context.unicodeScalars.count))
        #expect(hit.locator.spine == hit.spine)
    }

    // A hit is a place to go and a range to paint, which is the whole
    // reason both travel on it.
    let hit = try #require(hits.first)
    #expect(try session.go(to: hit.locator))
    try session.select(hit.locators)
    #expect(try session.selectedRange() == hit.locators)

    // One unit is the worker-drivable half, and it narrows.
    let firstUnit = try session.searchUnit(0, for: "chapter")
    #expect(!firstUnit.isEmpty)
    #expect(firstUnit.allSatisfy { $0.spine == 0 })

    #expect(try session.search("wordthatisnotinthebook").isEmpty)
}

// MARK: Fonts, and the rest

@Test func theTypefaceIsThePickersToChange() throws {
    let (session, dir) = try reader("fonts")
    defer { try? FileManager.default.removeItem(at: dir) }

    let families = try session.fontFamilies()
    #expect(families.contains("Crimson Text"), "the embedded family is matchable")
    #expect(families == families.sorted(), "sorted, for a picker to show as-is")

    // Unset reads as the publisher's own, not as an empty string.
    #expect(session.fontFamily() == nil)
    try session.setFontFamily("Crimson Text", scope: .thisBook)
    #expect(session.fontFamily() == "Crimson Text")
    _ = try session.pageCount()  // the reflow the choice causes

    try session.setFontFamily(nil, scope: .thisBook)
    #expect(session.fontFamily() == nil, "back to the publisher's")
}

@Test func aFontSourceComposesDirectoriesAndGenerics() throws {
    let dir = try scratch("font-source")
    defer { try? FileManager.default.removeItem(at: dir) }

    // The builders compose onto a preset rather than replacing it, and
    // the resulting source still opens a book.
    let source = FontSource
        .embedded(directory: fixtures.appendingPathComponent("fonts"), family: "Crimson Text")
        .addingDirectory(fixtures.appendingPathComponent("fonts"))
        .usingPlatformGenerics()
    let session = try Session(
        source: .path(fixtures.appendingPathComponent("epub/minimal.epub")),
        configuration: SessionConfiguration(fonts: source, libraryDirectory: dir))
    #expect(try session.fontFaceCount() > 0)

    let named = FontSource
        .embedded(directory: fixtures.appendingPathComponent("fonts"), family: "Crimson Text")
        .settingGenerics(
            FontSource.Generics(
                serif: "Crimson Text", sansSerif: "Crimson Text", monospace: "Crimson Text",
                cursive: "Crimson Text", fantasy: "Crimson Text"))
    let second = try Session(
        source: .path(fixtures.appendingPathComponent("epub/minimal.epub")),
        configuration: SessionConfiguration(fonts: named, libraryDirectory: dir))
    #expect(try second.fontFaceCount() > 0)
    // All five named at a family that is present, so nothing is left
    // dangling.
    #expect(second.unresolvedFontGenerics() == nil)
}

@Test func theBookSaysWhatKindItIsAndWhatItMayCache() throws {
    let (text, textDir) = try reader("kind-epub")
    defer { try? FileManager.default.removeItem(at: textDir) }
    #expect(try text.bookKind() == .epub)
    #expect(try text.cacheBudget() > 0)
    #expect(try text.cacheBytes() <= text.cacheBudget())

    let (comic, comicDir) = try reader("kind-comic", book: "cbz/minimal.cbz")
    defer { try? FileManager.default.removeItem(at: comicDir) }
    #expect(try comic.bookKind() == .comic)
}

@Test func zoomIsForPicturesAndNotForProse() throws {
    let (text, textDir) = try reader("zoom-text")
    defer { try? FileManager.default.removeItem(at: textDir) }

    // Reflowable text answers false: the same pinch means "bigger text"
    // there, which is a settings change the app makes itself.
    #expect(try !text.setPageZoom(2, focus: CGPoint(x: 300, y: 400)))
    #expect(try text.pageZoom() == 1)

    let (comic, comicDir) = try reader("zoom-comic", book: "cbz/minimal.cbz")
    defer { try? FileManager.default.removeItem(at: comicDir) }

    #expect(try comic.pageZoom() == 1, "fit, until something zooms")
    #expect(try !comic.panPage(by: CGSize(width: 10, height: 10)), "no pan at fit")

    #expect(try comic.setPageZoom(2, focus: CGPoint(x: 300, y: 400)))
    #expect(try comic.pageZoom() == 2)
    #expect(try comic.panPage(by: CGSize(width: -20, height: -20)))
    let pan = try comic.pagePan()
    #expect(pan != CGPoint.zero, "the pan moved off the origin")

    // Back to fit, and the pan goes with it.
    #expect(try comic.setPageZoom(1, focus: CGPoint(x: 300, y: 400)))
    #expect(try comic.pageZoom() == 1)
}

@Test func theHostCanWriteIntoTheEnginesOwnStream() throws {
    let lines = Lines()
    EngineLog.install(minimum: .info) { level, target, message in
        lines.append(level: level, target: target, message: message)
    }
    defer { EngineLog.remove() }

    #expect(EngineLog.isEnabled)
    EngineLog.write("the app has something to say")
    EngineLog.write("and something to say about itself", level: .warn, target: "demo")

    let seen = lines.all
    #expect(seen.contains { $0.message == "the app has something to say" && $0.target == "host" })
    #expect(
        seen.contains {
            $0.message == "and something to say about itself" && $0.target == "demo"
                && $0.level == .warn
        })

    EngineLog.remove()
    #expect(!EngineLog.isEnabled)
}

/// The log sink fires on any thread, so collect under a lock.
private final class Lines: @unchecked Sendable {
    struct Line {
        let level: EngineLog.Level
        let target: String
        let message: String
    }

    private let lock = NSLock()
    private var lines: [Line] = []

    func append(level: EngineLog.Level, target: String, message: String) {
        lock.withLock { lines.append(Line(level: level, target: target, message: message)) }
    }

    var all: [Line] { lock.withLock { lines } }
}
