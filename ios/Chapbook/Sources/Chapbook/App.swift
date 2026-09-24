import CChapbook
import Foundation

/// The app's secret store, asked by key.
///
/// The engine derives the key — `App.credentialKey(for:)` gives the same
/// one for the app's own lookups — and never parses what is stored
/// under it: a complete `Authorization` value, whatever the catalog took.
/// Calls arrive on whichever thread the layer is on, the catalog's or
/// the sync driver's, and must not prompt: a lookup is a lookup.
public protocol CredentialStore: AnyObject, Sendable {
    /// The stored value, or `nil` for nothing stored.
    func get(_ key: String) -> String?
    func set(_ key: String, authorization: String)
    func forget(_ key: String)
}

/// What the reader's progress readout says, beside the whole-book bar.
public enum ProgressLabel: String, CaseIterable, Sendable {
    /// "34%" of the whole book.
    case percent
    /// "6 left in chapter" — how many pages remain in this unit.
    case pagesLeft
    /// "unit 6/20 · page 2/11" — the raw indices.
    case chapterPage

    init(raw: cb_progress_label) {
        switch raw {
        case CB_PROGRESS_PAGES_LEFT: self = .pagesLeft
        case CB_PROGRESS_CHAPTER_PAGE: self = .chapterPage
        default: self = .percent
        }
    }

    var raw: cb_progress_label {
        switch self {
        case .percent: CB_PROGRESS_PERCENT
        case .pagesLeft: CB_PROGRESS_PAGES_LEFT
        case .chapterPage: CB_PROGRESS_CHAPTER_PAGE
        }
    }
}

/// Where the reader is, for the chrome, as one value read after a draw —
/// which is when a position is authoritative, because a restored
/// position lands on the first frame rather than at open.
public struct Place: Hashable, Sendable {
    public let spine: Int
    public let spineLength: Int
    public let page: Int
    public let pageCount: Int
    /// Whole-book progress, 0...1, spine-weighted like the engine's own
    /// progression.
    public let bookFraction: Double
    /// Pages after this one in the current unit.
    public let pagesLeft: Int
    /// Whether the engine's Back has anywhere to go — what decides
    /// whether a *Return* is drawn.
    public let canGoBack: Bool
}

/// What opening a shelf row came to.
public enum Opened {
    /// The library's own copy, open.
    case session(Session)
    /// The platform's file: read the grant with `App.grant(for:)` by the
    /// row's fingerprint, resolve it, and open with `App.open(_:)`.
    case adopted
    /// Out of reach: the copy is gone, or an adopted book whose grant was
    /// never kept. The row survives; say the file is out of reach.
    case missing
}

/// How a platform transfer's HTTP status is read.
public enum DownloadOutcome: Hashable, Sendable {
    /// The file is the book. Land it with `App.landDownload`.
    case landed
    /// 401 or 403: worth a sign-in, not a retry with the same credential.
    case refused
    /// 404, 410, or any other client-side answer: retrying will not
    /// change its mind.
    case gone
    /// A 5xx, or no response at all: retry with backoff.
    case again

    /// Read a status the way every front end reads it.
    public static func of(status: Int) -> DownloadOutcome {
        switch cb_download_outcome_of_status(UInt16(clamping: status)) {
        case CB_DOWNLOAD_LANDED: .landed
        case CB_DOWNLOAD_REFUSED: .refused
        case CB_DOWNLOAD_GONE: .gone
        default: .again
        }
    }
}

/// The application: what the app used to write for itself in Swift,
/// written once in the engine and reached from here.
///
/// Custody (which door a file comes in through, and how the book is
/// found again), the reader's place and memory rules, the search walk,
/// saved catalogs, browsing a catalog through its login, what a landed
/// download does, and sync credentialed per book from the app's
/// `CredentialStore`. The platform stays the app's: the Keychain behind
/// the store, `URLSession` behind the transport and the transfer, the
/// security-scoped bookmark that is a grant, the memory figure to hand
/// `Reader.cacheBudget(for:)`. Answers come back as enums and numbers;
/// the app has the strings.
///
/// Like `Session`, deliberately **not** `Sendable`: it holds a library
/// connection, so it is one isolation domain's at a time and may be
/// transferred, never shared. It coexists with a `Library`, sessions and
/// catalogs over the same directory. Letting the last reference go joins
/// the sync driver if one was started.
public final class App {
    let raw: OpaquePointer

    /// Open the application over `libraryDirectory`, on the platform
    /// named: the app's fonts, its store, its transport, and the device
    /// name a progression service shows beside this device's position.
    public init(
        libraryDirectory: URL,
        fonts: FontSource,
        credentials: CredentialStore,
        transport: HTTPTransport = .platformDefault,
        deviceName: String
    ) throws {
        let fontsRaw = try fonts.makeRaw()
        guard let config = cb_config_new(fontsRaw) else { throw ChapbookError.openFailure() }
        do {
            try check(cb_config_set_library_dir(config, libraryDirectory.path))
            // The store crosses as three callbacks over a retained box;
            // the engine runs the finalizer exactly once, on the same
            // rule as the transport's.
            let box = Unmanaged.passRetained(CredentialStoreBox(credentials))
            try check(
                cb_config_set_credential_store(
                    config, credentialGet, credentialStore, credentialForget, credentialFinalize,
                    box.toOpaque()))
            try transport.installFull(into: config)
        } catch {
            cb_config_free(config)
            throw error
        }
        var handle: OpaquePointer?
        // Consumed either way.
        try check(cb_app_open(config, deviceName, &handle))
        guard let handle else { throw ChapbookError.openFailure() }
        raw = handle
    }

    deinit {
        // Joins the sync driver if one was started — when this returns
        // nothing is still inside a callback, which is what makes
        // releasing the waker box safe on the next line.
        cb_app_close(raw)
        waker?.release()
    }

    // MARK: Credential helpers

    /// The key a credential for `url` lives under — scheme, host and any
    /// explicit port, never the path, which may itself be a secret. What
    /// the app's own code asks its store for, so it agrees with what the
    /// layer stored. `nil` for anything that is not a URL with an origin.
    public static func credentialKey(for url: URL) -> String? {
        credentialKey(for: url.absoluteString)
    }

    public static func credentialKey(for url: String) -> String? {
        readString { cb_credential_key(url, $0, $1, $2) }
    }

    /// The `Authorization` value for HTTP Basic.
    public static func basicAuthorization(username: String, password: String) -> String {
        readString { cb_basic_authorization(username, password, $0, $1, $2) } ?? ""
    }

    // MARK: Sessions

    /// Open a session over a source with the app's own configuration —
    /// how an adopted book is read once its grant has been resolved, and
    /// how a book is opened once to be recorded.
    public func open(_ source: BookSource, cacheBudgetBytes: Int? = nil) throws -> Session {
        guard let config = cb_app_session_config(raw) else { throw ChapbookError.openFailure() }
        if let budget = cacheBudgetBytes {
            do {
                try check(cb_config_set_cache_budget(config, budget))
            } catch {
                cb_config_free(config)
                throw error
            }
        }
        return try Session(source: source, rawConfig: config)
    }

    // MARK: Custody

    /// Import: copy a file into the library and answer with its row.
    /// Blocking.
    public func importFile(at url: URL) throws -> Int64 {
        var book: Int64 = 0
        try check(cb_app_import(raw, url.path, &book))
        return book
    }

    /// Adopt: record a book the platform owns, from a descriptor the
    /// session takes ownership of, and remember `grant` — a
    /// security-scoped bookmark — as the way to reach it again. The
    /// library keeps no copy. The same bytes adopted twice are one row.
    /// Blocking.
    public func adopt(fileDescriptor fd: Int32, format: BookFormat = .guess, grant: Data) throws -> Int64 {
        var book: Int64 = 0
        try grant.withUnsafeBytes { bytes in
            try check(
                cb_app_adopt_fd(
                    raw, fd, format.rawValue, bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count, &book))
        }
        return book
    }

    /// `adopt(fileDescriptor:format:grant:)` for a session already open
    /// through `open(_:cacheBudgetBytes:)`.
    public func adopt(_ session: Session, grant: Data) throws -> Int64 {
        var book: Int64 = 0
        try grant.withUnsafeBytes { bytes in
            try check(
                cb_app_adopt(
                    raw, session.raw, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count,
                    &book))
        }
        return book
    }

    /// The grant that reaches an adopted book, by the row's fingerprint,
    /// or `nil` when none was remembered.
    public func grant(for fingerprint: String) throws -> Data? {
        var needed = 0
        let probe = cb_app_grant(raw, fingerprint, nil, 0, &needed)
        if probe == C.unavailable { return nil }
        guard probe == C.bufferTooSmall else { throw ChapbookError.last(probe) }
        var bytes = [UInt8](repeating: 0, count: needed)
        try check(cb_app_grant(raw, fingerprint, &bytes, bytes.count, &needed))
        return Data(bytes)
    }

    public func remember(grant: Data, for fingerprint: String) throws {
        try grant.withUnsafeBytes { bytes in
            try check(
                cb_app_remember_grant(
                    raw, fingerprint, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count))
        }
    }

    public func forgetGrant(for fingerprint: String) throws {
        try check(cb_app_forget_grant(raw, fingerprint))
    }

    /// Open a shelf row for reading, whichever door it came in through.
    /// A row that has left the shelf throws. Blocking.
    public func open(book: Int64) throws -> Opened {
        var session: OpaquePointer?
        var how = CB_OPENED_MISSING
        try check(cb_app_open_book(raw, book, &session, &how))
        switch how {
        case CB_OPENED_SESSION:
            guard let session else { throw ChapbookError.openFailure() }
            return .session(Session(raw: session))
        case CB_OPENED_ADOPTED:
            return .adopted
        default:
            return .missing
        }
    }

    // MARK: Preferences

    /// The reader's chosen readout, kept for every launch.
    public var progressLabel: ProgressLabel {
        get {
            var label = CB_PROGRESS_PERCENT
            guard cb_app_progress_label(raw, &label) == C.ok else { return .percent }
            return ProgressLabel(raw: label)
        }
        set { _ = cb_app_set_progress_label(raw, newValue.raw) }
    }

    // MARK: Catalogs

    /// A catalog the reader added: where it is and what it called itself.
    public struct SavedCatalog: Hashable, Sendable, Identifiable {
        public let id: Int64
        /// Blank until the reader names it or its feed does.
        public let title: String
        /// Opaque and possibly secret-bearing: never log it, never key a
        /// credential by it.
        public let url: String
    }

    /// The catalogs the reader has added, in the order they were added.
    public func catalogs() throws -> [SavedCatalog] {
        var count = 0
        try check(cb_app_catalog_count(raw, &count))
        var out: [SavedCatalog] = []
        out.reserveCapacity(count)
        for index in 0..<count {
            var id: Int64 = 0
            try check(cb_app_catalog_id(raw, index, &id))
            out.append(
                SavedCatalog(
                    id: id,
                    title: try readOptionalString {
                        cb_app_catalog_text(raw, index, CB_SAVED_CATALOG_TITLE, $0, $1, $2)
                    } ?? "",
                    url: try readOptionalString {
                        cb_app_catalog_text(raw, index, CB_SAVED_CATALOG_URL, $0, $1, $2)
                    } ?? ""))
        }
        return out
    }

    /// Add a catalog; `title` may be empty until its feed says what it is
    /// called. Surrounding whitespace is the reader's typing and goes.
    public func addCatalog(url: String, title: String = "") throws -> SavedCatalog {
        var id: Int64 = 0
        try check(cb_app_add_catalog(raw, url, title, &id))
        return SavedCatalog(
            id: id, title: title.trimmingCharacters(in: .whitespaces),
            url: url.trimmingCharacters(in: .whitespaces))
    }

    public func renameCatalog(_ id: Int64, to title: String) throws {
        try check(cb_app_rename_catalog(raw, id, title))
    }

    /// Take a catalog off the list. Its books stay, and so does its
    /// credential.
    public func removeCatalog(_ id: Int64) throws {
        try check(cb_app_remove_catalog(raw, id))
    }

    /// Browse a saved catalog — or, with `nil`, no row: a pasted URL — as
    /// a `Catalog` over the app's transport and store, titled after the
    /// saved row until its feed says otherwise. The handle belongs to
    /// whichever isolation domain does the blocking fetches and does not
    /// need this app to stay alive.
    public func browse(_ id: Int64?) throws -> Catalog {
        var handle: OpaquePointer?
        try check(cb_app_browse(raw, id ?? 0, &handle))
        guard let handle else { throw ChapbookError.openFailure() }
        return Catalog(raw: handle)
    }

    // MARK: Downloads

    /// Everything a landed download does: import the file the transfer
    /// produced and record the sync services the entry advertised, read
    /// before the transfer. The file is the caller's and is left where it
    /// was; the same bytes twice are one row, so a retried job needs no
    /// bookkeeping. Blocking.
    public func landDownload(at file: URL, progressionURL: URL?, annotationContainer: URL?) throws -> Int64 {
        var book: Int64 = 0
        try withOptionalCString(progressionURL?.absoluteString) { progression in
            try withOptionalCString(annotationContainer?.absoluteString) { container in
                try check(cb_app_land_download(raw, file.path, progression, container, &book))
            }
        }
        return book
    }

    // MARK: Sync

    private var waker: Unmanaged<WakerBox>?

    /// Ask for every book with a service to reconcile, starting the app's
    /// driver on first use over the app's transport, credentialed per
    /// book from the app's store. `false` — and nothing asked — when no
    /// book on the shelf has a service. `onWake` fires on the driver's
    /// thread once per report and must only nudge the main actor to come
    /// `drainSyncReports()`; the first one given is the one kept.
    public func syncAll(onWake: (@Sendable () -> Void)? = nil) throws -> Bool {
        var started = false
        let (wake, user) = wakeArguments(onWake)
        try check(cb_app_sync_all(raw, wake, user, &started))
        return started
    }

    public func syncBook(_ book: Int64, onWake: (@Sendable () -> Void)? = nil) throws {
        let (wake, user) = wakeArguments(onWake)
        try check(cb_app_sync_book(raw, book, wake, user))
    }

    private func wakeArguments(_ onWake: (@Sendable () -> Void)?) -> (cb_wake_fn?, UnsafeMutableRawPointer?) {
        guard let onWake, waker == nil else { return (nil, nil) }
        let box = Unmanaged.passRetained(WakerBox(onWake))
        waker = box
        return (syncWake, box.toOpaque())
    }

    /// Everything sync reported since the last drain, oldest first.
    public func drainSyncReports() throws -> [SyncReport] {
        var reports: [SyncReport] = []
        while true {
            var report = cb_sync_report()
            let status = cb_app_sync_next(raw, &report)
            if status == C.unavailable { break }
            try check(status)
            reports.append(SyncReport(raw: report))
        }
        return reports
    }
}

/// The reader's policy over a session: the decisions a reading screen
/// makes that are not about drawing, each written in the engine rather
/// than once per app.
public enum Reader {
    /// How much of what the platform says this process may use goes to a
    /// session's cache: a quarter of `availableBytes`, between a floor
    /// and a cap. Hand `os_proc_available_memory()`.
    public static func cacheBudget(for availableBytes: UInt64) -> Int {
        cb_reader_cache_budget_for(availableBytes)
    }
}

extension Session {
    /// Where the reader is, read after a draw. Lays the current unit out
    /// if nothing has yet.
    public func place() throws -> Place {
        var place = cb_place()
        try check(cb_reader_place(raw, &place))
        return Place(
            spine: place.spine, spineLength: place.spine_len, page: place.page,
            pageCount: place.page_count, bookFraction: place.book_fraction,
            pagesLeft: place.pages_left, canGoBack: place.can_go_back)
    }

    /// What a memory warning does: halve the budget, which evicts at
    /// once, and release the caches. The budget now in force.
    public func afterMemoryWarning() throws -> Int {
        var budget = 0
        try check(cb_reader_after_memory_warning(raw, &budget))
        return budget
    }

    /// Jump to a hit and leave it selected, so the eye finds it. Whether
    /// the position moved.
    public func show(_ hit: SearchHit) throws -> Bool {
        var record = cb_search_hit(
            spine: hit.spine, start: hit.locators.lowerBound, end: hit.locators.upperBound,
            match_start: hit.matchInContext.lowerBound, match_end: hit.matchInContext.upperBound)
        var moved = false
        try check(cb_reader_show_hit(raw, &record, &moved))
        return moved
    }

    /// The selection becomes a highlight, and the selection goes. `nil`
    /// with nothing selected.
    public func highlightSelection() throws -> Int64? {
        var id: Int64 = 0
        let status = cb_reader_highlight_selection(raw, &id)
        if status == C.unavailable { return nil }
        try check(status)
        return id
    }

    /// The selection becomes a note, and the selection goes. `nil` with
    /// nothing selected.
    public func note(onSelection body: String) throws -> Int64? {
        var id: Int64 = 0
        let status = cb_reader_note_on_selection(raw, body, &id)
        if status == C.unavailable { return nil }
        try check(status)
        return id
    }
}

/// A search walked one unit at a time on the session's own isolation
/// domain, so the page stays responsive between steps: call `step(_:)`
/// from a task on the main actor, `await Task.yield()` between units,
/// and read `hits()` after each. It stops itself at a cap past which a
/// results list is a scroll nobody finishes. Not `Sendable`, like the
/// session it walks.
public final class SearchWalk {
    let raw: OpaquePointer

    /// Begin a search. `nil` for a query that is only whitespace, which
    /// clears rather than searches.
    public init?(query: String) {
        var handle: OpaquePointer?
        guard cb_search_walk_open(query, &handle) == C.ok, let handle else { return nil }
        raw = handle
    }

    deinit { cb_search_walk_close(raw) }

    /// Search the next unit. Whether there is another to search.
    public func step(_ session: Session) throws -> Bool {
        var more = false
        try check(cb_search_walk_step(raw, session.raw, &more))
        return more
    }

    /// Every hit so far, in reading order.
    public func hits() throws -> [Session.SearchHit] {
        var count = 0
        try check(cb_search_walk_hit_count(raw, &count))
        var found: [Session.SearchHit] = []
        found.reserveCapacity(count)
        for index in 0..<count {
            var record = cb_search_hit(spine: 0, start: 0, end: 0, match_start: 0, match_end: 0)
            try check(cb_search_walk_hit(raw, index, &record))
            found.append(
                Session.SearchHit(
                    spine: record.spine,
                    locators: record.start..<record.end,
                    context: try readOptionalString {
                        cb_search_walk_context(self.raw, index, $0, $1, $2)
                    } ?? "",
                    matchInContext: record.match_start..<record.match_end))
        }
        return found
    }
}

// MARK: The credential store, crossing C

/// The retained object behind the C `user` pointer, released by the
/// engine's finalizer exactly once.
final class CredentialStoreBox: Sendable {
    let store: CredentialStore
    init(_ store: CredentialStore) { self.store = store }
}

func credentialGet(
    key: UnsafePointer<CChar>?, response: OpaquePointer?, user: UnsafeMutableRawPointer?
) {
    guard let key, let user else { return }
    let store = Unmanaged<CredentialStoreBox>.fromOpaque(user).takeUnretainedValue().store
    if let value = store.get(String(cString: key)) {
        _ = cb_credential_response_found(response, value)
    }
}

func credentialStore(
    key: UnsafePointer<CChar>?, authorization: UnsafePointer<CChar>?, user: UnsafeMutableRawPointer?
) -> Int32 {
    guard let key, let authorization, let user else { return Int32(CB_ERR_NULL_ARGUMENT.rawValue) }
    let store = Unmanaged<CredentialStoreBox>.fromOpaque(user).takeUnretainedValue().store
    store.set(String(cString: key), authorization: String(cString: authorization))
    return C.ok
}

func credentialForget(key: UnsafePointer<CChar>?, user: UnsafeMutableRawPointer?) -> Int32 {
    guard let key, let user else { return Int32(CB_ERR_NULL_ARGUMENT.rawValue) }
    let store = Unmanaged<CredentialStoreBox>.fromOpaque(user).takeUnretainedValue().store
    store.forget(String(cString: key))
    return C.ok
}

func credentialFinalize(user: UnsafeMutableRawPointer?) {
    guard let user else { return }
    Unmanaged<CredentialStoreBox>.fromOpaque(user).release()
}
