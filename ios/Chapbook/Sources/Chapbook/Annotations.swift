import CChapbook
import CoreGraphics
import Foundation

// The marks a reader leaves: bookmarks, highlights, notes. These persist
// in the library and travel to a book's annotation container on the next
// sync, so a removal here is a removal everywhere — nothing about them is
// local scratch state.
//
// Two things have to be true before a mark can exist, and they fail
// differently. The build needs the library capability
// (`Capabilities.current().contains(.library)`), or every call here
// throws saying a mark nothing would remember is not a mark; and the
// session needs a `libraryDirectory`, without which there is nowhere to
// put one. Neither is checked for you.
extension Session {
    /// What kind of mark a row is.
    public enum AnnotationKind: Hashable, Sendable {
        /// A point remembered, nothing painted.
        case bookmark
        /// A range painted on the page.
        case highlight
        /// A range with words attached.
        case note

        init(raw: cb_annotation_kind) {
            switch raw {
            case CB_ANNOTATION_HIGHLIGHT: self = .highlight
            case CB_ANNOTATION_NOTE: self = .note
            default: self = .bookmark
            }
        }
    }

    /// One mark on this book.
    public struct Annotation: Hashable, Sendable, Identifiable {
        /// Stable for the mark's life, sync included — which is why every
        /// mutating call takes this and not an index.
        public let id: Int64
        public let kind: AnnotationKind
        /// The spine unit the mark resolves against in this book.
        public let spine: Int
        /// Whole-book progression of its start, 0...1 — what orders a
        /// marks list and places its gutter dots.
        public let progression: Double
        /// The quoted text of a highlight, or the body of a note. `nil`
        /// for a bookmark, which has neither.
        public let text: String?
        /// The chosen color as the hex string it was set with, or `nil`
        /// for a mark wearing the theme's.
        public let color: String?
    }

    /// This book's marks, ordered by progression.
    ///
    /// Re-read from the library per call, so **re-enumerate after any add
    /// or remove**: indices are stable between mutations and not across
    /// them. Ids are stable throughout, which is what to hold onto.
    public func annotations() throws -> [Annotation] {
        var count = 0
        try check(cb_session_annotation_count(raw, &count))
        var marks: [Annotation] = []
        marks.reserveCapacity(count)
        for index in 0..<count {
            var record = cb_annotation(
                id: 0, kind: CB_ANNOTATION_BOOKMARK, spine: 0, progression: 0,
                has_text: false, has_color: false)
            try check(cb_session_annotation(raw, index, &record))
            marks.append(
                Annotation(
                    id: record.id,
                    kind: AnnotationKind(raw: record.kind),
                    spine: record.spine,
                    progression: record.progression,
                    text: record.has_text
                        ? readString { cb_session_annotation_text(self.raw, index, $0, $1, $2) }
                        : nil,
                    color: record.has_color
                        ? readString { cb_session_annotation_color(self.raw, index, $0, $1, $2) }
                        : nil))
        }
        return marks
    }

    // MARK: Making them

    /// Bookmark the current position — a point, nothing painted.
    ///
    /// Throws when there is no position to keep yet: a session with no
    /// metrics has not laid anything out, so there is nowhere to point.
    @discardableResult
    public func addBookmark() throws -> Int64 {
        var id: Int64 = 0
        try check(cb_session_add_bookmark(raw, &id))
        return id
    }

    /// Turn the live selection into a stored highlight, in the theme's
    /// color until one is chosen.
    ///
    /// The selection is still the app's afterwards: clearing it is a
    /// separate move, so the paint order — highlight replaces selection —
    /// is explicit rather than implied.
    ///
    /// Throws when there is nothing to make a highlight out of: nothing
    /// selected, no library to keep it in, or an image book, which has no
    /// text to cover. Reported rather than returned as a quiet `nil`,
    /// because a `nil` from a mutating call is discardable and a highlight
    /// button that does nothing forever is what that produces.
    @discardableResult
    public func addHighlight() throws -> Int64 {
        var id: Int64 = 0
        try check(cb_session_add_highlight(raw, &id))
        return id
    }

    /// Turn the live selection into a note carrying `body`. Throws on
    /// the same conditions as [`addHighlight()`].
    @discardableResult
    public func addNote(_ body: String) throws -> Int64 {
        var id: Int64 = 0
        try check(cb_session_add_note(raw, body, &id))
        return id
    }

    // MARK: Touching them

    /// The highlight under a point in logical panel coordinates, or `nil`
    /// on a miss — what a tap on marked text asks before the app opens
    /// its recolor-or-remove menu.
    ///
    /// Ask it **after [`link(at:)`] and before the tap zones**: highlights
    /// are exact, so a miss falls through to the turn band naturally.
    public func highlight(at point: CGPoint) throws -> Int64? {
        var id: Int64 = 0
        let status = cb_session_highlight_at(raw, Float(point.x), Float(point.y), &id)
        if status == C.unavailable { return nil }
        try check(status)
        return id
    }

    /// Recolor a highlight. `color` is `"#rrggbb"` or `"#rrggbbaa"`;
    /// `nil` gives the theme's color back.
    public func setHighlightColor(_ color: String?, for id: Int64) throws {
        try withOptionalCString(color) { color in
            try check(cb_session_set_highlight_color(self.raw, id, color))
        }
    }

    /// Remove a mark, whatever its kind. The removal reaches the book's
    /// annotation container on the next sync; nothing here is silent.
    public func removeAnnotation(_ id: Int64) throws {
        try check(cb_session_remove_annotation(raw, id))
    }

    /// Jump to a mark, pushing the return position for `Action.back` the
    /// way a followed link does. Returns whether the reader went
    /// anywhere.
    @discardableResult
    public func go(toAnnotation id: Int64) throws -> Bool {
        var moved = false
        try check(cb_session_goto_annotation(raw, id, &moved))
        return moved
    }

    @discardableResult
    public func go(to annotation: Annotation) throws -> Bool {
        try go(toAnnotation: annotation.id)
    }
}
