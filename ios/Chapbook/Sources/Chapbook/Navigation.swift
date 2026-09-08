import CChapbook
import CoreGraphics
import Foundation

// Getting somewhere on purpose: the contents, the durable locator, the
// jumps, and the links a reader taps.
//
// **The jumps push the return position for `Action.back`; the skips do
// not.** Pushing are `go(to:)` in each of its three forms,
// `go(toAnchor:inSpine:)`, `go(toAnnotation:)` and `follow(link:)`. Not
// pushing are the page turns and `nextUnit()`/`prevUnit()` below — a
// chapter skip is a reader walking through the book, not a departure
// from somewhere they meant to come back to, and if it pushed, "back"
// would spend itself undoing navigation the reader did on purpose.
// `canGoBack()` is what greys out the button.
extension Session {
    // MARK: The contents

    /// One entry of the table of contents, flattened — `depth` carries
    /// the nesting a menu indents by.
    ///
    /// `id` is the entry's index in this book's contents, which is what
    /// `go(to:)` takes and what makes the type `Identifiable` for a
    /// list. Labels repeat across a book; indices do not.
    public struct TOCEntry: Hashable, Sendable, Identifiable {
        public let id: Int
        public let label: String
        /// 0 for a top-level entry, 1 for its children.
        public let depth: Int
        /// The spine unit it points at, or `nil` for an entry that named
        /// none when the contents were read — typically a heading that
        /// links nowhere.
        ///
        /// Not the same question as "can I jump to it": the jump also
        /// resolves an entry's href, so `nil` here and a successful
        /// `go(to:)` is an ordinary pairing. Show them all and let the
        /// jump answer.
        public let spine: Int?
        /// Whether it points inside its unit rather than at the start.
        /// Nothing to act on; the jump handles it either way.
        public let pointsWithinUnit: Bool
    }

    /// The book's contents in reading order. Empty for a book with none,
    /// which is ordinary — a comic has no contents.
    public func tableOfContents() throws -> [TOCEntry] {
        var count = 0
        try check(cb_session_toc_count(raw, &count))
        var entries: [TOCEntry] = []
        entries.reserveCapacity(count)
        for index in 0..<count {
            var record = cb_toc_entry(depth: 0, spine: 0, has_spine: false, has_fragment: false)
            try check(cb_session_toc_entry(raw, index, &record))
            entries.append(
                TOCEntry(
                    id: index,
                    label: try readOptionalString {
                        cb_session_toc_label(self.raw, index, $0, $1, $2)
                    } ?? "",
                    depth: record.depth,
                    spine: record.has_spine ? record.spine : nil,
                    pointsWithinUnit: record.has_fragment))
        }
        return entries
    }

    /// Jump to a contents entry. `false` for one that resolves to no
    /// unit by either its spine index or its href — a section heading —
    /// which is not an error. Throws only if the entry is no longer
    /// there, which means the contents were re-read since.
    @discardableResult
    public func go(to entry: TOCEntry) throws -> Bool {
        var moved = false
        try check(cb_session_goto_toc(raw, entry.id, &moved))
        return moved
    }

    // MARK: Locators

    /// Where the reader is, as a place in the text: a spine unit and a
    /// character offset into it.
    ///
    /// The durable half of the pair, and the one to save. The library
    /// stores this, marks anchor to it and sync carries it, while
    /// [`Session.position()`] counts pages in the *current* pagination —
    /// so a position saved at one font size means somewhere else at
    /// another, and a locator still means this passage.
    ///
    /// The offset is where the current page begins, so it lands on a page
    /// boundary rather than on the exact character last looked at, and a
    /// reflow can move it within the same passage. What holds across both
    /// is the round trip: hand one back to `go(to:)` and the reader is
    /// where they were.
    public struct Locator: Hashable, Sendable {
        public let spine: Int
        public let offset: UInt32

        public init(spine: Int, offset: UInt32) {
            self.spine = spine
            self.offset = offset
        }
    }

    /// Where the reader is now.
    ///
    /// Before the first [`setMetrics(_:)`] there is no pagination to ask,
    /// and this answers offset 0 rather than refusing — so an app that
    /// persists a place on an early suspend saves the top of the unit
    /// and gets no signal that it did. Set metrics first, or do not save
    /// what an unlaid-out session reports.
    public func locator() throws -> Locator {
        var spine = 0
        var offset: UInt32 = 0
        try check(cb_session_locator(raw, &spine, &offset))
        return Locator(spine: spine, offset: offset)
    }

    /// Jump to a locator — a place saved earlier, a search hit, a
    /// position another device reached. `false` for a spine index the
    /// book does not have; an offset past the unit's text lands at its
    /// end rather than failing.
    @discardableResult
    public func go(to locator: Locator) throws -> Bool {
        var moved = false
        try check(cb_session_goto(raw, locator.spine, locator.offset, &moved))
        return moved
    }

    /// Jump to an element id within a unit — a footnote, a
    /// cross-reference. A fragment the unit does not carry lands at the
    /// unit's start rather than failing, so `false` means only that the
    /// book has no such spine index.
    @discardableResult
    public func go(toAnchor fragment: String, inSpine spine: Int) throws -> Bool {
        var moved = false
        try check(cb_session_goto_anchor(raw, spine, fragment, &moved))
        return moved
    }

    // MARK: Units

    /// Skip to the start of the next spine unit — a chapter skip.
    /// Returns whether the position moved.
    ///
    /// Leaves the back trail alone, like a page turn and unlike a jump.
    @discardableResult
    public func nextUnit() throws -> Bool {
        var moved = false
        try check(cb_session_next_unit(raw, &moved))
        return moved
    }

    @discardableResult
    public func prevUnit() throws -> Bool {
        var moved = false
        try check(cb_session_prev_unit(raw, &moved))
        return moved
    }

    // MARK: The back trail

    /// Whether [`Action.back`] has anywhere to return to — a 64-deep
    /// stack that jumps push and page turns do not. The question a back
    /// button's enabled state asks; applying the action is the answer.
    public func canGoBack() throws -> Bool {
        var can = false
        try check(cb_session_can_go_back(raw, &can))
        return can
    }

    // MARK: Links

    /// The link under a point, as the href the book wrote, or `nil` when
    /// the point is not on one.
    ///
    /// `point` is in logical panel coordinates, like every hit test here.
    /// **Ask this before starting a selection and before the tap zones**:
    /// links are exact, so a miss falls through naturally, and asking in
    /// the other order lets the turn band swallow every link in the outer
    /// thirds of the page — which reads as "links don't work in this app"
    /// rather than as a precedence bug.
    public func link(at point: CGPoint) throws -> String? {
        try readOptionalString {
            cb_session_link_at(self.raw, Float(point.x), Float(point.y), $0, $1, $2)
        }
    }

    /// Follow an href — one [`link(at:)`] answered, or a contents
    /// entry's. `false` for anything that is not a reading position: an
    /// external `http(s)` link is the app's to open in a browser, and
    /// that answer is the app's opportunity rather than a failure.
    @discardableResult
    public func follow(link href: String) throws -> Bool {
        var moved = false
        try check(cb_session_follow_link(raw, href, &moved))
        return moved
    }
}
