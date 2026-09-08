import CChapbook
import Foundation

// Finding words in the book. Two entry points with the same result type:
// the whole spine at once, and one unit at a time for a shell that wants
// results as they arrive.
extension Session {
    /// One search hit: where it is in the book, and enough context to
    /// draw a results row.
    public struct SearchHit: Hashable, Sendable {
        /// The unit the match is in.
        public let spine: Int
        /// The match in locator space — hand it to
        /// [`Session.select(_:)`] after jumping, which is how a hit gets
        /// painted on the page.
        public let locators: Range<UInt32>
        /// The match with a little text either side, whitespace
        /// collapsed.
        public let context: String
        /// Where the match sits within `context`, so a results list can
        /// embolden the matched words rather than the whole line.
        ///
        /// **Unicode scalar offsets, not `Character` offsets.** The
        /// engine counts in scalars and Swift's `Character` is a grapheme
        /// cluster, so index through `context.unicodeScalars` — reaching
        /// for `context.index(_:offsetBy:)` gets the right answer until
        /// the line holds an emoji or a combining mark, and then quietly
        /// emboldens the wrong words.
        public let matchInContext: Range<UInt32>

        /// Where to jump before selecting: the hit's own start.
        public var locator: Locator {
            Locator(spine: spine, offset: locators.lowerBound)
        }
    }

    /// Search the whole book, keeping at most `limit` hits — 0 for the
    /// engine's own sane cap.
    ///
    /// **Blocking and potentially slow**: it lays nothing out, but it
    /// reads and folds every unit's text. A responsive search box runs
    /// this off the main actor, or walks units itself with
    /// [`searchUnit(_:for:)`].
    ///
    /// The engine holds the hits until the next search or the session's
    /// close, and this reads them all out before returning, so the array
    /// outlives both.
    public func search(_ query: String, limit: Int = 0) throws -> [SearchHit] {
        var count = 0
        try check(cb_session_search(raw, query, limit, &count))
        return try hits(count)
    }

    /// Search one unit — the worker-drivable half, for results as they
    /// arrive. Replaces whatever the last search left, exactly as
    /// [`search(_:limit:)`] does.
    public func searchUnit(_ spine: Int, for query: String) throws -> [SearchHit] {
        var count = 0
        try check(cb_session_search_unit(raw, spine, query, &count))
        return try hits(count)
    }

    private func hits(_ count: Int) throws -> [SearchHit] {
        var found: [SearchHit] = []
        found.reserveCapacity(count)
        for index in 0..<count {
            var record = cb_search_hit(
                spine: 0, start: 0, end: 0, match_start: 0, match_end: 0)
            try check(cb_session_search_hit(raw, index, &record))
            found.append(
                SearchHit(
                    spine: record.spine,
                    locators: record.start..<record.end,
                    context: try readOptionalString {
                        cb_session_search_context(self.raw, index, $0, $1, $2)
                    } ?? "",
                    matchInContext: record.match_start..<record.match_end))
        }
        return found
    }
}
