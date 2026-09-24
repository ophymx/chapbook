import Chapbook
import Foundation
#if canImport(os)
    import os
#endif

/// Where the reader is, for the chrome. Updated after every draw, from
/// the engine's own reading of it (`Session.place()`); this only adds
/// the title.
public struct Place: Hashable, Sendable {
    public var title = ""
    public var spine = 0
    public var spineLength = 0
    public var page = 0
    public var pageCount = 0
    /// Whole-book progress, 0...1, spine-weighted like the engine's own
    /// progression.
    public var bookFraction = 0.0
    /// Whether the engine's Back has anywhere to go — after a link.
    public var canGoBack = false
}

/// One open book, and what was read once at open.
public struct Reading {
    public let session: Session
    public let book: Library.Book
    public let kind: BookKind
    /// Read once at open; a book's contents do not change.
    public let contents: [Session.TOCEntry]
    /// Every family the session's fonts offer, for the picker.
    public let fontFamilies: [String]
}

public enum ReaderState {
    case opening
    case reading(Reading)
    case gone
}

/// A search in progress or finished.
public struct SearchState: Sendable {
    public var query = ""
    public var hits: [Session.SearchHit] = []
    public var running = false
}

/// The reader's text selection, as the chrome sees it.
public struct Selection: Hashable, Sendable {
    public let locators: Range<UInt32>
    public let text: String
}

/// One open book, for as long as its screen exists.
///
/// The session lives here rather than in the view because a rotation
/// re-lays the view out and must not reopen the book: the model outlives
/// the view, the session moves to whichever page view is current, and it
/// is closed exactly once, when the screen is popped. The engine's rule
/// is that a session is touched by one thread at a time: it is opened on
/// a background queue and, once handed over, only ever touched from the
/// main actor — which is why a search here walks the book unit by unit
/// on the main actor, yielding between units, rather than blocking a
/// worker that would race the next draw.
///
/// Everything that changes what the page shows ends by asking the view
/// to redraw through `onNeedsRedraw`; the view installs that while it is
/// on screen.
@MainActor
public final class ReaderViewModel: ObservableObject {
    @Published public private(set) var state: ReaderState = .opening
    @Published public private(set) var place = Place()
    @Published public private(set) var settings: ReadingSettings?
    @Published public private(set) var fontFamily: String?
    @Published public private(set) var marks: [Session.Annotation] = []
    @Published public private(set) var search = SearchState()
    @Published public private(set) var selection: Selection?

    /// Installed by the page while it is showing.
    public var onNeedsRedraw: (() -> Void)?

    private let bookID: Int64
    private let shelf: Shelf
    private let opener: Opener
    public let preferences: Preferences
    private var searching: Task<Void, Never>?

    public var session: Session? {
        if case .reading(let reading) = state { return reading.session }
        return nil
    }

    public init(bookID: Int64, shelf: Shelf, opener: Opener, preferences: Preferences) {
        self.bookID = bookID
        self.shelf = shelf
        self.opener = opener
        self.preferences = preferences
        Task { [weak self] in await self?.open() }
    }

    private func open() async {
        guard let book = try? await shelf.book(bookID) else {
            state = .gone
            return
        }
        // The engine's default budget is a desktop's; the platform says
        // what this process may take and the engine says how much of
        // that a page cache is worth.
        let budget = Self.memoryBudget()
        let opener = self.opener
        let opened = await offMain { () throws -> Reading? in
            guard let session = try opener.open(book, cacheBudget: budget) else { return nil }
            // Read on the same thread that opened, before the hand-over:
            // the contents and the font list do not change and both
            // cost a little.
            return Reading(
                session: session, book: book, kind: try session.bookKind(),
                contents: try session.tableOfContents(), fontFamilies: try session.fontFamilies())
        }
        guard case .success(.some(let reading)) = opened else {
            if case .failure(let error) = opened {
                EngineLog.write("open failed: \(error)", level: .error, target: "reader")
            }
            state = .gone
            return
        }
        settings = try? reading.session.settings()
        fontFamily = reading.session.fontFamily()
        marks = (try? reading.session.annotations()) ?? []
        state = .reading(reading)
    }

    static func memoryBudget() -> Int {
        #if os(iOS)
            return Reader.cacheBudget(for: UInt64(os_proc_available_memory()))
        #else
            return Reader.cacheBudget(for: ProcessInfo.processInfo.physicalMemory / 4)
        #endif
    }

    private func redraw() {
        onNeedsRedraw?()
    }

    // MARK: Where the reader is

    /// Called by the page after each draw, which is when a position is
    /// authoritative.
    public func moved(_ position: Session.Position) {
        guard let session, let read = try? session.place() else { return }
        _ = position
        place = Place(
            title: session.title() ?? "",
            spine: read.spine,
            spineLength: read.spineLength,
            page: read.page,
            pageCount: read.pageCount,
            bookFraction: read.bookFraction,
            canGoBack: read.canGoBack)
        // Settings can change under a page turn — `fontUp` from a pinch
        // — so the sheet's numbers follow the draw too.
        settings = try? session.settings()
    }

    /// The engine's Back: where the reader was before the last link.
    public func goBack() {
        guard let session, (try? session.apply(.back)) == .changed else { return }
        redraw()
    }

    public func go(to entry: Session.TOCEntry) {
        guard let session, (try? session.go(to: entry)) == true else { return }
        redraw()
    }

    // MARK: Settings

    public func apply(_ settings: ReadingSettings, thisBook: Bool) {
        guard let session else { return }
        try? session.setSettings(settings, scope: thisBook ? .thisBook : .global)
        self.settings = try? session.settings()
        redraw()
    }

    public func setFontFamily(_ family: String?, thisBook: Bool) {
        guard let session else { return }
        try? session.setFontFamily(family, scope: thisBook ? .thisBook : .global)
        fontFamily = session.fontFamily()
        redraw()
    }

    /// Drop this book's own settings so it follows the defaults again.
    public func resetBookSettings() {
        guard let session else { return }
        try? session.clearBookSettings()
        settings = try? session.settings()
        fontFamily = session.fontFamily()
        redraw()
    }

    // MARK: Marks

    private func refreshMarks() {
        marks = (try? session?.annotations()) ?? []
    }

    public func addBookmark() {
        _ = try? session?.addBookmark()
        refreshMarks()
    }

    /// The selection becomes a highlight, and the selection goes.
    public func highlightSelection() {
        guard let session else { return }
        _ = try? session.highlightSelection()
        selection = nil
        refreshMarks()
        redraw()
    }

    public func note(onSelection body: String) {
        guard let session else { return }
        _ = try? session.note(onSelection: body)
        selection = nil
        refreshMarks()
        redraw()
    }

    public func removeMark(_ id: Int64) {
        try? session?.removeAnnotation(id)
        refreshMarks()
        redraw()
    }

    public func go(toMark id: Int64) {
        guard let session, (try? session.go(toAnnotation: id)) == true else { return }
        redraw()
    }

    public func recolorHighlight(_ id: Int64, color: String?) {
        try? session?.setHighlightColor(color, for: id)
        refreshMarks()
        redraw()
    }

    // MARK: Selection

    /// The page reports what is selected after each draw; `nil` clears.
    public func selected(_ locators: Range<UInt32>?) {
        guard let session, let locators else {
            selection = nil
            return
        }
        selection = Selection(locators: locators, text: ((try? session.selectedText()) ?? nil) ?? "")
    }

    public func clearSelection() {
        if let session, ((try? session.selectedRange()) ?? nil) != nil {
            try? session.clearSelection()
            redraw()
        }
        selection = nil
    }

    // MARK: Search

    /// Search the book, stepping the engine's walk one unit at a time on
    /// the main actor and yielding between units so the page stays
    /// responsive. The blocking whole-book `search` would want a worker,
    /// and a worker would touch the session while the page draws — the
    /// one rule the engine has. The cap is the walk's.
    public func search(_ query: String) {
        searching?.cancel()
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        guard let walk = SearchWalk(query: trimmed) else {
            search = SearchState()
            return
        }
        search = SearchState(query: trimmed, running: true)
        searching = Task { [weak self] in
            guard let self, let session else { return }
            var more = true
            while more {
                if Task.isCancelled { return }
                more = (try? walk.step(session)) ?? false
                search.hits = (try? walk.hits()) ?? []
                await Task.yield()
            }
            if !Task.isCancelled { search.running = false }
        }
    }

    public func clearSearch() {
        searching?.cancel()
        search = SearchState()
    }

    /// Jump to a hit and leave it selected, so the eye finds it.
    public func go(toHit hit: Session.SearchHit) {
        guard let session else { return }
        _ = try? session.show(hit)
        redraw()
    }

    // MARK: Lifecycle

    /// `didEnterBackground`: the last callback the platform guarantees.
    public func suspend() {
        try? session?.suspend()
    }

    /// A memory warning: the engine halves the budget, which evicts at
    /// once and keeps the next warning from finding the same cache.
    public func memoryWarning() {
        _ = try? session?.afterMemoryWarning()
    }

    /// The screen is gone: save the place and let the session go, once.
    public func close() {
        searching?.cancel()
        if let session {
            try? session.savePosition()
        }
        state = .gone
    }
}
