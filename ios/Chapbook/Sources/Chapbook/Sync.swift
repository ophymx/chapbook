import CChapbook
import Foundation

/// What happened to one book's reading position when it reconciled.
public enum PositionOutcome: Hashable, Sendable {
    /// Nothing to do: no service, or nothing had changed on either side.
    case idle
    /// This device's position reached the service.
    case pushed
    /// The service's position was adopted locally.
    case pulled
    /// The service declined; what it holds is newer. Not a failure — the
    /// next pull brings it down if the local copy is clean by then.
    case refused
    /// Both sides moved since they last agreed. Nothing was overwritten.
    case conflict
    /// The service could not be reached, or answered something unusable.
    /// Nothing local changed.
    case failed

    init(raw: cb_sync_position) {
        switch raw {
        case CB_SYNC_POSITION_IDLE: self = .idle
        case CB_SYNC_POSITION_PUSHED: self = .pushed
        case CB_SYNC_POSITION_PULLED: self = .pulled
        case CB_SYNC_POSITION_REFUSED: self = .refused
        case CB_SYNC_POSITION_CONFLICT: self = .conflict
        default: self = .failed
        }
    }
}

/// One report from the worker, as [`SyncWorker.nextReport()`] drains
/// them.
public enum SyncReport: Hashable, Sendable {
    /// A book reconciled. Failures *inside* it — an unreachable service,
    /// a refused write — live in the report's `position`, `detail` and
    /// `marksError`, because one dead host must not read as a dead batch.
    case book(BookReport)
    /// A book did not reconcile at all — removed from the shelf, or no
    /// service to talk to. The rest of the batch still ran.
    case bookFailed(book: Int64, reason: String)
    /// A batch finished; `books` says how many reports preceded this.
    /// The signal to stop showing a spinner.
    case finished(books: Int)
    /// A batch could not start — the library would not answer. Only
    /// `App.drainSyncReports()` reports it; a `SyncWorker` has its
    /// library by then.
    case broken(reason: String)

    /// What one book's reconcile did, half position and half marks.
    public struct BookReport: Hashable, Sendable {
        /// The library row, `Library.Book.id`'s space.
        public let book: Int64
        public let position: PositionOutcome
        /// The position's refusal or failure explained, when there is one.
        public let detail: String?
        /// Marks: written to the container as new.
        public let marksCreated: Int
        /// Marks: this device's edits written over the container's copy.
        public let marksUpdated: Int
        /// Marks: deletes carried out on the container.
        public let marksDeleted: Int
        /// Marks: pulled from the container as marks this device had not
        /// seen.
        public let marksAdopted: Int
        /// Marks: already known here, brought up to date with what
        /// another device wrote.
        public let marksRefreshed: Int
        /// Marks: another device's deletions arriving — taken off this
        /// shelf because a complete container listing no longer holds
        /// them. Distinct from `marksDeleted`, this device's own
        /// deletions reaching the container.
        public let marksWithdrawn: Int
        /// Marks: conflicts settled by re-reading the container — both
        /// edits survive, nothing overwritten.
        public let marksMerged: Int
        /// Marks: still owing a write after a merge was attempted. The
        /// next pass tries again.
        public let marksConflicts: Int
        /// The container had more pages than one pass reads: the pull
        /// saw a prefix and no deletion was inferred, so a mark another
        /// device removed may still be sitting here. Changes what the
        /// counts above mean, which is why it is worth showing.
        public let listingTruncated: Bool
        /// The container could not be reached; whatever was pushed before
        /// it failed stands. `nil` when the mark half ran to the end.
        public let marksError: String?
    }
}

/// A sync worker over one library, on its own thread.
///
/// Ask with [`requestAll()`] or [`requestBook(_:)`]; reports arrive
/// through [`nextReport()`] or [`drainReports()`], one per book and then
/// a [`SyncReport.finished(books:)`]. Like `Session`, deliberately not
/// `Sendable`: the handle is movable between isolation domains and never
/// shareable across them — the worker's own thread reaches back only
/// through the waker.
///
/// The worker holds its own connection to the library, so it coexists
/// with open sessions and a `Library` on the same directory. Letting the
/// last reference go joins the worker thread, which blocks for the book
/// in flight — release it from something that can afford the wait.
public final class SyncWorker {
    let raw: OpaquePointer
    private let waker: Unmanaged<WakerBox>?

    /// Open a worker over the library at `libraryDirectory` — the same
    /// URL the sessions were configured with.
    ///
    /// `deviceID` is how a progression service tells this device's
    /// positions from another's: mint one once (a `UUID().uuidString`
    /// into the keychain or defaults), store it, pass the same one
    /// forever. `deviceName` is for people.
    ///
    /// The transport is the same choice a `SessionConfiguration` makes,
    /// with the same default — `URLSession` on iOS, the bundled Rust
    /// transport on macOS. Requests carry no credentials from the engine;
    /// a service behind auth wants a `URLSession` whose configuration
    /// attaches them.
    ///
    /// `onWake` fires **on the worker thread** once per queued report and
    /// must only nudge the main actor to come drain; `nil` means the app
    /// polls on its own clock.
    public init(
        libraryDirectory: URL,
        deviceID: String,
        deviceName: String,
        transport: HTTPTransport = .platformDefault,
        onWake: (@Sendable () -> Void)? = nil
    ) throws {
        let wakerBox = onWake.map { Unmanaged.passRetained(WakerBox($0)) }
        let wake: cb_wake_fn? = wakerBox == nil ? nil : syncWake

        var handle: OpaquePointer?
        let status: Int32
        switch transport.kind {
        case .bundled:
            // Both callbacks null asks for the bundled transport; the
            // open declines honestly in a build without one.
            status = cb_sync_open(
                libraryDirectory.path, deviceID, deviceName,
                nil, nil, nil, nil,
                wake, wakerBox?.toOpaque(), &handle)
        case .urlSession(let session):
            // On failure the engine has already run the finalizer — its
            // ownership rule — so the box needs no release of its own.
            let box = Unmanaged.passRetained(URLSessionTransport(session: session))
            status = cb_sync_open(
                libraryDirectory.path, deviceID, deviceName,
                transportGet, transportSend, transportFinalize, box.toOpaque(),
                wake, wakerBox?.toOpaque(), &handle)
        }
        guard status == C.ok, let handle else {
            wakerBox?.release()
            throw ChapbookError.last(status)
        }
        raw = handle
        waker = wakerBox
    }

    deinit {
        // Joins the worker thread — when this returns nothing is still
        // inside a callback, which is what makes releasing the waker box
        // safe on the next line.
        cb_sync_close(raw)
        waker?.release()
    }

    /// Ask for every book with a service to reconcile. A shelf where
    /// nothing syncs finishes immediately with zero books, which is a
    /// fact to tell the reader rather than a spinner to show them.
    public func requestAll() throws {
        try check(cb_sync_request_all(raw))
    }

    /// Ask for one book. A book with no service, or one that has left
    /// the shelf, reports [`SyncReport.bookFailed(book:reason:)`] rather
    /// than being silently skipped.
    public func requestBook(_ book: Int64) throws {
        try check(cb_sync_request_book(raw, book))
    }

    /// The next report, oldest first, or `nil` when there is none — the
    /// ordinary answer between wakes, not an error.
    public func nextReport() throws -> SyncReport? {
        var report = cb_sync_report()
        let status = cb_sync_next(raw, &report)
        if status == C.unavailable { return nil }
        try check(status)
        return SyncReport(raw: report)
    }

    /// Everything reported since the last drain, oldest first.
    public func drainReports() throws -> [SyncReport] {
        var reports: [SyncReport] = []
        while let report = try nextReport() { reports.append(report) }
        return reports
    }
}

extension SyncReport {
    /// One report read out of its C form. The strings are borrowed from
    /// the handle that produced it and die on the next call;
    /// `String(cString:)` copies them out here. Shared by `SyncWorker`
    /// and `App`, which fill the same struct.
    init(raw report: cb_sync_report) {
        switch report.kind {
        case CB_SYNC_BOOK:
            self = .book(
                SyncReport.BookReport(
                    book: report.book,
                    position: PositionOutcome(raw: report.position),
                    detail: report.detail.map { String(cString: $0) },
                    marksCreated: report.marks_created,
                    marksUpdated: report.marks_updated,
                    marksDeleted: report.marks_deleted,
                    marksAdopted: report.marks_adopted,
                    marksRefreshed: report.marks_refreshed,
                    marksWithdrawn: report.marks_withdrawn,
                    marksMerged: report.marks_merged,
                    marksConflicts: report.marks_conflicts,
                    listingTruncated: report.listing_truncated,
                    marksError: report.marks_error.map { String(cString: $0) }))
        case CB_SYNC_BOOK_FAILED:
            self = .bookFailed(
                book: report.book,
                reason: report.detail.map { String(cString: $0) } ?? "")
        case CB_SYNC_BROKEN:
            self = .broken(reason: report.detail.map { String(cString: $0) } ?? "")
        default:
            self = .finished(books: report.books)
        }
    }
}

func syncWake(user: UnsafeMutableRawPointer?) {
    guard let user else { return }
    Unmanaged<WakerBox>.fromOpaque(user).takeUnretainedValue().handler()
}

extension Library {
    /// Record where a book syncs: its OPDS Progression endpoint and its
    /// Web Annotation container, either or both — the two service links
    /// off the catalog entry it was downloaded from, which is the only
    /// place a book learns them. Calling again replaces both values; two
    /// `nil`s make the book local again without touching what it still
    /// owes.
    ///
    /// Both URLs are opaque and may embed a per-user key: never log
    /// them, and key any credential by origin, not by URL.
    public func setSyncTargets(
        book: Int64, progressionURL: URL?, annotationContainer: URL?
    ) throws {
        try withOptionalCString(progressionURL?.absoluteString) { progression in
            try withOptionalCString(annotationContainer?.absoluteString) { container in
                try check(cb_library_set_sync_targets(self.raw, book, progression, container))
            }
        }
    }

    /// The progression service this book syncs its position to, or `nil`
    /// for none — the ordinary state of a sideloaded book.
    public func syncProgressionURL(book: Int64) -> URL? {
        readString { cb_library_sync_progression_url(self.raw, book, $0, $1, $2) }
            .flatMap { URL(string: $0) }
    }

    /// The Web Annotation container this book syncs its marks with, or
    /// `nil` for none.
    public func syncAnnotationContainer(book: Int64) -> URL? {
        readString { cb_library_sync_annotation_container(self.raw, book, $0, $1, $2) }
            .flatMap { URL(string: $0) }
    }
}
