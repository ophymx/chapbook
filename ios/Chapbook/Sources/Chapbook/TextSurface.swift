import CChapbook
import CoreGraphics
import Foundation

// The text surface: the current page's text with geometry — what an
// accessibility element tree (`UIAccessibilityElement`), speech
// (`AVSpeechSynthesizer` word callbacks), or a dictionary popup consumes.
//
// Offsets and geometry both live in the engine's spaces: locator ranges
// are the same offsets positions and annotations use, and rects are page
// space (CSS px, page top-left). This shell's logical view coordinates
// match page space while the metrics carry no rotation.
extension Session {
    /// One visual line of the current page: its geometry, locator range,
    /// and text. The text's length is *not* the locator span — shaped
    /// text collapses whitespace, drops soft hyphens, adds marks.
    public struct TextRun: Hashable, Sendable {
        public let rect: CGRect
        public let locators: Range<UInt32>
        public let text: String
    }

    /// One word: where it sits in the speakable string and in locator
    /// space. Speech progress reports index the string; the locator range
    /// is how they become a highlight via `rects(for:)`.
    public struct WordSpan: Hashable, Sendable {
        /// **Unicode scalar offsets into `SpeakablePage.text`, not
        /// `Character` offsets** — index through `text.unicodeScalars`,
        /// since a `Character` is a grapheme cluster and the engine
        /// counts scalars.
        public let textRange: Range<UInt32>
        public let locators: Range<UInt32>
    }

    /// The page as a speech engine wants it: one collapsed string plus
    /// the word table that maps progress back to the page.
    public struct SpeakablePage: Hashable, Sendable {
        public let text: String
        public let words: [WordSpan]
    }

    /// The current page's text runs in reading order, or `nil` before the
    /// page is laid out. Empty for a page with nothing to speak (a comic).
    /// Re-fetch after anything that redraws — a turn, a reflow.
    public func pageTextRuns() throws -> [TextRun]? {
        var count = 0
        let status = cb_session_page_text_run_count(raw, &count)
        if status == C.unavailable { return nil }
        try check(status)
        var runs: [TextRun] = []
        runs.reserveCapacity(count)
        for index in 0..<count {
            var record = cb_text_run(
                rect: cb_rect(x: 0, y: 0, w: 0, h: 0), locator_start: 0, locator_end: 0)
            try check(cb_session_page_text_run(raw, index, &record))
            let text =
                readString { cb_session_page_text_run_text(self.raw, index, $0, $1, $2) } ?? ""
            runs.append(
                TextRun(
                    rect: CGRect(
                        x: CGFloat(record.rect.x), y: CGFloat(record.rect.y),
                        width: CGFloat(record.rect.w), height: CGFloat(record.rect.h)),
                    locators: record.locator_start..<record.locator_end,
                    text: text))
        }
        return runs
    }

    /// The page's speakable string and word table, or `nil` before the
    /// page is laid out.
    public func speakablePage() throws -> SpeakablePage? {
        var count = 0
        let status = cb_session_page_word_count(raw, &count)
        if status == C.unavailable { return nil }
        try check(status)
        let text = readString { cb_session_page_speakable_text(self.raw, $0, $1, $2) } ?? ""
        var words: [WordSpan] = []
        words.reserveCapacity(count)
        for index in 0..<count {
            var span = cb_word_span(text_start: 0, text_end: 0, locator_start: 0, locator_end: 0)
            try check(cb_session_page_word(raw, index, &span))
            words.append(
                WordSpan(
                    textRange: span.text_start..<span.text_end,
                    locators: span.locator_start..<span.locator_end))
        }
        return SpeakablePage(text: text, words: words)
    }

    /// Page-space rects covering a locator range on the current page —
    /// one per line the range touches. Empty when the page is not laid
    /// out or the range lies elsewhere.
    public func rects(for locators: Range<UInt32>) throws -> [CGRect] {
        var needed = 0
        let probe = cb_session_range_rects(
            raw, locators.lowerBound, locators.upperBound, nil, 0, &needed)
        if probe == C.ok { return [] }
        guard probe == C.bufferTooSmall else { throw ChapbookError.last(probe) }
        var buf = [cb_rect](repeating: cb_rect(x: 0, y: 0, w: 0, h: 0), count: needed)
        try check(
            cb_session_range_rects(
                raw, locators.lowerBound, locators.upperBound, &buf, buf.count, &needed))
        return buf.prefix(needed).map {
            CGRect(
                x: CGFloat($0.x), y: CGFloat($0.y),
                width: CGFloat($0.w), height: CGFloat($0.h))
        }
    }

    /// The word under a point in logical panel coordinates, as a locator
    /// range — dictionary lookup's question. `nil` off text, on
    /// whitespace, or on bare punctuation.
    public func word(at point: CGPoint) throws -> Range<UInt32>? {
        var start: UInt32 = 0
        var end: UInt32 = 0
        let status = cb_session_word_at(raw, Float(point.x), Float(point.y), &start, &end)
        if status == C.unavailable { return nil }
        try check(status)
        return start..<end
    }
}
