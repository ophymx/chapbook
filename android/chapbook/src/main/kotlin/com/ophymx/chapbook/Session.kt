package com.ophymx.chapbook

import android.graphics.Bitmap
import android.os.ParcelFileDescriptor

/** Where the reader is. The pair, never the page alone — see `docs/SHELLS.md`. */
data class Position(val spine: Int, val page: Int)

/** What kind of book a session opened. */
enum class BookKind { EPUB, COMIC, PDF }

/** The durable position: a unit, and a character offset within it. */
data class Locator(val spine: Int, val offset: Int)

/** One flattened table-of-contents entry. */
data class TocEntry(
    val label: String,
    /** 0 for a top-level entry, 1 for its children. */
    val depth: Int,
    /** The unit it points at, or null for a heading that links nowhere. */
    val spine: Int?,
    /** Whether it points inside its unit rather than at the start. */
    val hasFragment: Boolean,
    /** Its place in the flattened list — what [Session.gotoToc] takes. */
    val index: Int,
)

/** One search hit. */
data class SearchHit(
    val spine: Int,
    /** Locator offsets of the match — hand these to [Session.selectRange]. */
    val start: Int,
    val end: Int,
    /** The match with a little text either side, for a results list. */
    val context: String,
    /** Char range of the match within [context]. */
    val matchStart: Int,
    val matchEnd: Int,
)

/** What kind of mark a row is. */
enum class AnnotationKind { BOOKMARK, HIGHLIGHT, NOTE }

/** One mark, as the marks list shows it. */
data class Annotation(
    val id: Long,
    val kind: AnnotationKind,
    /** The spine unit it resolves against. */
    val spine: Int,
    /** Whole-book progression of its start, 0.0..=1.0. */
    val progression: Double,
    /** A highlight's quote or a note's body. */
    val text: String?,
    /** The chosen color, or null for the theme's. */
    val color: String?,
)

/** The engine's colour themes, in cycle order. */
enum class Theme { LIGHT, SEPIA, DARK }

/**
 * The scalar reading settings. The font family deliberately travels on
 * its own calls — see [Session.setSettings].
 */
data class ReadingSettings(
    val baseFontPx: Float,
    val lineHeight: Float,
    val justify: Boolean,
    val publisherStyles: Boolean,
    val theme: Theme,
)

/**
 * Something the session wants a shell to know, drained from
 * [Session.drainEvents]. Deliberately not about drawing — what to repaint
 * is [Session.pollLoaded]'s answer.
 */
sealed class SessionEvent {
    /** A background unit finished decoding, prefetches included. */
    data class UnitLoaded(val spine: Int) : SessionEvent()

    /** A background unit failed and will not be retried. [message] is for a person. */
    data class UnitFailed(val spine: Int, val message: String) : SessionEvent()

    /** The reader is somewhere else — moves the shell did not make included. */
    data class PositionChanged(val spine: Int, val page: Int) : SessionEvent()

    /** The reader reached the last page of the last unit. */
    data object BookFinished : SessionEvent()
}

/** The device-pixel size a bitmap must be before [Session.renderInto] will draw. */
data class RenderSize(val width: Int, val height: Int)

/**
 * One visual line of the current page, with its geometry and locator
 * range — the material an `AccessibilityNodeInfo` tree is built from.
 * The rect is page space (logical units, page top-left); the locator
 * range is `[start, end)` in the same offsets positions and annotations
 * use. The text's length is not the locator span's width.
 */
data class TextRun(
    val text: String,
    val rect: android.graphics.RectF,
    val locatorStart: Int,
    val locatorEnd: Int,
)

/**
 * One word: where it sits in [Session.speakableText] (char offsets) and
 * in locator space. `onRangeStart` from a TTS utterance reports offsets
 * into the string; the locator range is how that progress becomes a
 * highlight via [Session.rangeRects].
 */
data class WordSpan(
    val textStart: Int,
    val textEnd: Int,
    val locatorStart: Int,
    val locatorEnd: Int,
)

/**
 * What the engine did with an action, and what you owe the platform back.
 *
 * Two questions, not one, and neither implies the other. [needsRedraw] says
 * whether to `invalidate()`. [consumed] says what to return from
 * `onKeyDown`/`onKeyUp` — and getting *that* wrong is expensive here,
 * because the default key map binds the volume keys to page turns: a
 * reader that answers "nothing changed" on the last page hands the press
 * back and Android draws its volume slider over the book.
 */
enum class ActionOutcome {
    /** Applied, and something moved. Repaint, and consume the event. */
    Changed,

    /** Applied, nothing moved — last page, font at its stop. Still yours. */
    Unchanged,

    /**
     * Not the engine's: `toggle-menu` always, and `back` with an empty
     * trail. Let the event through, which is how the system Back leaves
     * the reader without this side tracking the history to know when.
     */
    Unhandled;

    val needsRedraw: Boolean get() = this == Changed
    val consumed: Boolean get() = this != Unhandled
}

/**
 * One open book.
 *
 * Held by the app for as long as it is reading, and [close]d exactly once.
 * The underlying session is `Send` but not `Sync`: it may move between
 * threads, and must never be touched from two at the same time. This class
 * does not enforce that — the C ABI's header will say it, and a real
 * binding should.
 */
class Session private constructor(private var handle: Long) : AutoCloseable {

    companion object {
        /**
         * Send the engine's diagnostics to logcat under the tag `chapbook`.
         *
         * Call once, before opening anything. The engine is silent until a
         * host installs a backend, and on Android silent used to mean the
         * failures only a device hits were the ones nobody could see.
         */
        fun initLogging(verbose: Boolean = false) = Native.initLogging(verbose)

        /**
         * Opens a book from a filesystem path.
         *
         * `libraryDir` should be `context.filesDir` — it is where positions,
         * annotations and settings live, and it is an argument because a
         * sandboxed app's answer is not reachable any other way.
         *
         * Returns null if the book will not open; logcat says why.
         */
        fun open(path: String, libraryDir: String): Session? {
            val handle = Native.open(path, libraryDir)
            return if (handle == 0L) null else Session(handle)
        }

        /**
         * Opens a book from a `content://` URI's file descriptor — what the
         * storage access framework actually hands an app, with no path and
         * usually no extension. The format comes from the bytes.
         *
         * Takes ownership of [pfd]: it is detached here and closed by the
         * session, so the caller must not close it or use it again.
         *
         * A book opened this way reaches the library by content — hashed on
         * open, recorded under the same edition fingerprint a path import
         * gets — so its position and annotations persist. Reopening the
         * *file* next launch is the app's job: take a persistable URI
         * grant, re-resolve it, and hand the descriptor back here.
         */
        fun openFd(pfd: ParcelFileDescriptor, libraryDir: String): Session? {
            val handle = Native.openFd(pfd.detachFd(), libraryDir)
            return if (handle == 0L) null else Session(handle)
        }

        /** Run the conformance harness over a book. It opens its own sessions. */
        fun conformance(path: String, libraryDir: String): String =
            Native.conformance(path, libraryDir)
    }

    /**
     * What the font source produced: faces loaded, and any generic family
     * pointed at a name no loaded face carries.
     *
     * Zero faces means every page paginates blank, taking navigation,
     * search and the table of contents with it — still the single most
     * useful string on this screen.
     */
    val fontReport: String get() = Native.fontReport(handle)

    val title: String get() = Native.title(handle)

    val position: Position
        get() {
            val packed = Native.position(handle)
            return Position((packed ushr 32).toInt(), (packed and 0xffffffffL).toInt())
        }

    /** Null until [setMetrics] has been called. */
    val renderSize: RenderSize?
        get() {
            val packed = Native.renderSize(handle)
            if (packed < 0) return null
            return RenderSize((packed ushr 32).toInt(), (packed and 0xffffffffL).toInt())
        }

    val cacheBytes: Long get() = Native.cacheBytes(handle)
    val cacheBudget: Long get() = Native.cacheBudget(handle)

    fun setMetrics(width: Float, height: Float, margin: Float, scale: Float) =
        Native.setMetrics(handle, width, height, margin, scale)

    /** Returns whether the position moved. Do not derive this from [position]. */
    fun nextPage(): Boolean = Native.nextPage(handle)

    /** Returns whether the position moved. */
    fun prevPage(): Boolean = Native.prevPage(handle)

    /** Next theme. Repaints; the position does not move. */
    fun cycleTheme() = Native.cycleTheme(handle)

    // ---- Background loads ----
    //
    // Image books (CBZ, PDF) decode off the UI thread. Without this pair
    // wired, a comic opens to its placeholder page and stays there.

    /**
     * Install the wake callback, run once per landed load **on the loader
     * thread**. It must only get back to the main thread — `View.post`, a
     * `Handler` — and call [pollLoaded]; touching a view from inside it is
     * the bug every reference shell's waker comment warns about. Null
     * clears it.
     */
    fun setWaker(waker: Runnable?) = Native.setWaker(handle, waker)

    /**
     * Take delivery of anything the loader finished. Returns whether the
     * visible page changed, and therefore whether to repaint — prefetch
     * landings answer false on purpose.
     */
    fun pollLoaded(): Boolean = Native.pollLoaded(handle)

    /** Whether any unit is still loading — for a shell that shows a spinner. */
    val hasPendingLoads: Boolean get() = Native.hasPendingLoads(handle)

    // ---- Session events ----

    /**
     * Everything the session wants a shell to know since the last drain:
     * loads landing and failing, the position moving, the book finishing.
     * Drain after a wake or a draw; the engine coalesces on its side.
     */
    fun drainEvents(): List<SessionEvent> {
        val out = mutableListOf<SessionEvent>()
        while (true) {
            val packed = Native.nextEvent(handle)
            if (packed < 0) break
            val spine = ((packed ushr 28) and 0x0fff_ffff).toInt()
            val page = (packed and 0x0fff_ffff).toInt()
            out.add(
                when ((packed ushr 56).toInt()) {
                    0 -> SessionEvent.UnitLoaded(spine)
                    1 -> SessionEvent.UnitFailed(spine, Native.eventMessage(handle) ?: "")
                    2 -> SessionEvent.PositionChanged(spine, page)
                    else -> SessionEvent.BookFinished
                }
            )
        }
        return out
    }

    // ---- The rest of the reading model ----

    /** How many spine units the book has — the denominator of "ch 2/8". */
    val spineLen: Int get() = Native.spineLen(handle)

    /** How many pages the current unit laid out to; 0 until metrics arrive. */
    val pageCount: Int get() = Native.pageCount(handle)

    /** What kind of book — "ch" or "pg" in a title bar. */
    val kind: BookKind
        get() = when (Native.bookKind(handle)) {
            1 -> BookKind.COMIC
            2 -> BookKind.PDF
            else -> BookKind.EPUB
        }

    // ---- Settings ----

    /** The settings in force, resolved override-over-default. */
    val settings: ReadingSettings?
        get() {
            val values = Native.settings(handle)
            if (values.size < 5) return null
            return ReadingSettings(
                baseFontPx = values[0],
                lineHeight = values[1],
                justify = values[2] != 0f,
                publisherStyles = values[3] != 0f,
                theme = Theme.entries.getOrElse(values[4].toInt()) { Theme.LIGHT },
            )
        }

    /**
     * Replace the scalar settings. The chosen font family is preserved,
     * not cleared — it travels on [setFontFamily], the same split the C
     * ABI keeps and for the same reason. [thisBook] scopes the change to
     * this book instead of the reader default; both persist through the
     * library when the session has one.
     */
    fun setSettings(settings: ReadingSettings, thisBook: Boolean = false) =
        Native.setSettings(
            handle,
            settings.baseFontPx,
            settings.lineHeight,
            settings.justify,
            settings.publisherStyles,
            settings.theme.ordinal,
            thisBook,
        )

    /** The reader's chosen typeface, or null for the publisher's. */
    val fontFamily: String? get() = Native.fontFamily(handle)

    /** Choose a typeface — one of [fontFamilies] — or null to let go. */
    fun setFontFamily(family: String?, thisBook: Boolean = false) =
        Native.setFontFamily(handle, family, thisBook)

    /** Every family the session's font database offers — a picker's list. */
    val fontFamilies: Array<String> get() = Native.fontFamilies(handle)

    // ---- Selection, links and marks ----
    //
    // The gesture surface. Long-press → [selectWordAt]; the handles a
    // shell draws come from [rangeRects] over [selectedRange]; dragging
    // one is [selectRange] with the adjusted offsets; the result becomes
    // a highlight the library keeps and sync carries.

    /** Anchor a selection at a point. Returns whether text was there. */
    fun selectionBegin(x: Float, y: Float): Boolean = Native.selectionBegin(handle, x, y)

    /** Extend the selection — press-drag, or a moving handle. */
    fun selectionDrag(x: Float, y: Float) = Native.selectionDrag(handle, x, y)

    /** Select the word under a point — what a long press means. */
    fun selectWordAt(x: Float, y: Float): Boolean = Native.selectWordAt(handle, x, y)

    /** Select an exact locator range — a search hit, an adjusted handle. */
    fun selectRange(start: Int, end: Int) = Native.selectRange(handle, start, end)

    /** Drop the selection. */
    fun selectionClear() = Native.selectionClear(handle)

    /** The selection as a locator range, or null when there is none. */
    val selectedRange: IntRange?
        get() {
            val packed = Native.selectedRange(handle)
            if (packed < 0) return null
            return (packed ushr 32).toInt() until (packed and 0xffff_ffffL).toInt()
        }

    /** The selected text, collapsed the way a clipboard wants it. */
    val selectedText: String? get() = Native.selectedText(handle)

    /** The link under a point, or null — check before starting a selection. */
    fun linkAt(x: Float, y: Float): String? = Native.linkAt(handle, x, y)

    /**
     * Follow an href. Returns whether the reader moved; an external
     * `http(s)` link answers false and is the app's to open in a browser.
     */
    fun followLink(href: String): Boolean = Native.followLink(handle, href)

    /** The selection becomes a stored highlight; its id, or null. */
    fun addHighlight(): Long? = Native.addHighlight(handle).takeIf { it > 0 }

    /** The selection becomes a note carrying [body]; its id, or null. */
    fun addNote(body: String): Long? = Native.addNote(handle, body).takeIf { it > 0 }

    /** Bookmark the current position; its id, or null. */
    fun addBookmark(): Long? = Native.addBookmark(handle).takeIf { it > 0 }

    /** The stored highlight under a point, or null — the recolor-menu tap. */
    fun highlightAt(x: Float, y: Float): Long? =
        Native.highlightAt(handle, x, y).takeIf { it > 0 }

    /** Recolor a highlight — `"#rrggbb"`/`"#rrggbbaa"`, null for the theme's. */
    fun setHighlightColor(id: Long, color: String?) =
        Native.setHighlightColor(handle, id, color)

    /** Remove a mark; the removal reaches the container on the next sync. */
    fun removeAnnotation(id: Long) = Native.removeAnnotation(handle, id)

    /** Jump to a mark. Returns whether the reader moved. */
    fun gotoAnnotation(id: Long): Boolean = Native.gotoAnnotation(handle, id)

    // ---- Page zoom (image books) ----

    /**
     * The pinch: zoom around a focal point, `ScaleGestureDetector`'s
     * numbers straight in. Image books only — false on prose, where the
     * same gesture is a font-size change ([apply] with `font-up` /
     * `font-down`). Zoom is view state: nothing persists it, and it
     * survives a page turn (call `setPageZoom(1f, …)` on turn to reset).
     */
    fun setPageZoom(zoom: Float, focusX: Float, focusY: Float): Boolean =
        Native.setPageZoom(handle, zoom, focusX, focusY)

    /**
     * Pan the zoomed page by a drag delta, clamped at the edges. False
     * at fit — the drag then falls through to a selection or swipe.
     */
    fun panPage(dx: Float, dy: Float): Boolean = Native.panPage(handle, dx, dy)

    /** The current zoom, 1.0 at fit. */
    val pageZoom: Float get() = Native.pageZoom(handle)

    /**
     * The current pan in page units — with [pageZoom], the forward map
     * for overlays: `view = fit * zoom + pan`. Output geometry like
     * [rangeRects] stays in fit-page space on purpose.
     */
    val pagePan: Pair<Float, Float>
        get() {
            val values = Native.pagePan(handle)
            return if (values.size == 2) values[0] to values[1] else 0f to 0f
        }

    // ---- Contents, search and the locator ----

    /**
     * The contents, flattened into reading order with a depth per entry
     * — what a menu draws directly, and what a tree can still be rebuilt
     * from. Entries that link nowhere are kept: they are section
     * headings, and dropping them would orphan their children.
     */
    fun toc(): List<TocEntry> {
        val rows = Native.toc(handle)
        return (0 until rows.size / 4).map { index ->
            val base = index * 4
            TocEntry(
                label = Native.tocLabel(handle, index) ?: "",
                depth = rows[base].toInt(),
                spine = if (rows[base + 2] != 0L) rows[base + 1].toInt() else null,
                hasFragment = rows[base + 3] != 0L,
                index = index,
            )
        }
    }

    /** Jump to a contents entry. False for one that links nowhere. */
    fun gotoToc(entry: TocEntry): Boolean = Native.gotoToc(handle, entry.index)

    /**
     * Search the whole book, at most [limit] hits (0 for a sane cap).
     * **Blocking** — run it off the UI thread. Replaces the last
     * search's results.
     */
    fun search(query: String, limit: Int = 0): List<SearchHit> =
        readHits(Native.search(handle, query, limit))

    /** Search one unit — the half a worker can drive per unit. */
    fun searchUnit(spine: Int, query: String): List<SearchHit> =
        readHits(Native.searchUnit(handle, spine, query))

    private fun readHits(count: Int): List<SearchHit> {
        if (count <= 0) return emptyList()
        return (0 until count).mapNotNull { index ->
            val values = Native.searchHit(handle, index)
            if (values.size < 5) return@mapNotNull null
            SearchHit(
                spine = values[0].toInt(),
                start = values[1].toInt(),
                end = values[2].toInt(),
                context = Native.searchContext(handle, index) ?: "",
                matchStart = values[3].toInt(),
                matchEnd = values[4].toInt(),
            )
        }
    }

    /**
     * The durable position: the unit and the character offset in it.
     * This is what the library stores and marks anchor to, and it does
     * not move when the font size does — [position] is the view, and
     * that one does.
     */
    val locator: Locator?
        get() {
            val packed = Native.locator(handle)
            if (packed < 0) return null
            return Locator((packed ushr 32).toInt(), (packed and 0xffff_ffffL).toInt())
        }

    /** Jump to a locator — a saved place, a search hit, another device's. */
    fun goto(locator: Locator): Boolean =
        Native.gotoLocator(handle, locator.spine, locator.offset)

    /** Jump to an element id in a unit — a footnote, a cross-reference. */
    fun gotoAnchor(spine: Int, fragment: String): Boolean =
        Native.gotoAnchor(handle, spine, fragment)

    /** Whether Back has anywhere to go — what greys out a back button. */
    val canGoBack: Boolean get() = Native.canGoBack(handle)

    /** Drop this book's own settings; it follows the defaults again. */
    fun clearBookSettings() = Native.clearBookSettings(handle)

    /** Every mark this book carries, ordered by progression. */
    fun annotations(): List<Annotation> {
        val count = Native.annotationCount(handle)
        if (count <= 0) return emptyList()
        return (0 until count).mapNotNull { index ->
            val values = Native.annotation(handle, index)
            if (values.size < 4) return@mapNotNull null
            Annotation(
                id = values[0],
                kind = when (values[1]) {
                    1L -> AnnotationKind.HIGHLIGHT
                    2L -> AnnotationKind.NOTE
                    else -> AnnotationKind.BOOKMARK
                },
                spine = values[2].toInt(),
                progression = Double.fromBits(values[3]),
                text = Native.annotationText(handle, index),
                color = Native.annotationColor(handle, index),
            )
        }
    }

    /**
     * Save the position and drop everything reconstructible. Call from
     * `onStop`, which is the last callback Android guarantees.
     */
    fun suspend() = Native.suspendSession(handle)

    /** Call from `onTrimMemory`. The current page is rebuilt on the next draw. */
    fun releaseCaches() = Native.releaseCaches(handle)

    /**
     * Draws the current page into [bitmap], which must be `ARGB_8888` and
     * exactly [renderSize]. The engine rasterizes into the bitmap's own
     * pixels; nothing is copied. Returns 0, or a negative code described in
     * `chapbook-jni`.
     */
    fun renderInto(bitmap: Bitmap): Int = Native.renderInto(handle, bitmap)

    /**
     * Which edge this book reads from: `"ltr"` or `"rtl"`.
     *
     * The book declares it — EPUB's `page-progression-direction` — and the
     * tap zones already use it. This is here so a shell can show that it
     * did, because a correctly flipped RTL book and a bug look the same
     * from the outside.
     */
    val readingDirection: String get() = Native.readingDirection(handle)

    /**
     * Reconfigure the tap bands as fractions of the page width. [middle] is
     * an action name, or `""` for a band that does nothing.
     *
     * There is no direction parameter on purpose: that one is the book's.
     */
    fun setTapZones(prevFraction: Float, nextFraction: Float, middle: String) =
        Native.setTapZones(handle, prevFraction, nextFraction, middle)

    /**
     * What a tap means, or null.
     *
     * [x] and [y] are **logical units** — view pixels divided by the
     * display density, the same space [setMetrics] is given — in panel
     * coordinates. A rotated panel is undone on the engine's side.
     */
    fun tapAction(x: Float, y: Float): String? =
        Native.tapAction(handle, x, y).ifEmpty { null }

    /** What a key means, or null. Takes an `KeyEvent.KEYCODE_*` value. */
    fun actionForKeyCode(keyCode: Int): String? =
        Native.actionForKeyCode(handle, keyCode).ifEmpty { null }

    /**
     * Apply what a tap or a key meant. You do not have to know which
     * action it was — hand back what you were given and read the outcome.
     */
    fun apply(action: String): ActionOutcome = when (Native.applyAction(handle, action)) {
        0 -> ActionOutcome.Changed
        1 -> ActionOutcome.Unchanged
        else -> ActionOutcome.Unhandled
    }

    // The text surface: what an accessibility tree, TTS, or a dictionary
    // popup consumes. Re-fetch after anything that redraws — a turn, a
    // reflow, a settings change.

    /**
     * The current page's text runs in reading order, or null until the
     * page is laid out. Empty for a page with nothing to speak (a comic).
     */
    fun pageTextRuns(): List<TextRun>? {
        val count = Native.pageTextRunCount(handle)
        if (count < 0) return null
        return (0 until count).mapNotNull { index ->
            val packed = Native.pageTextRunRange(handle, index)
            val rect = Native.pageTextRunRect(handle, index)
            if (packed < 0 || rect.size != 4) return@mapNotNull null
            TextRun(
                text = Native.pageTextRunText(handle, index),
                rect = android.graphics.RectF(
                    rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]),
                locatorStart = (packed ushr 32).toInt(),
                locatorEnd = (packed and 0xffffffffL).toInt(),
            )
        }
    }

    /** The page as one string for a TTS utterance; `""` until laid out. */
    val speakableText: String get() = Native.speakableText(handle)

    /** The word table mapping TTS progress back to locator space. */
    fun pageWords(): List<WordSpan> {
        val flat = Native.pageWords(handle)
        return (flat.indices step 4).map { i ->
            WordSpan(flat[i], flat[i + 1], flat[i + 2], flat[i + 3])
        }
    }

    /**
     * The word under a panel point as a locator range, or null — off
     * text, on whitespace, on bare punctuation. Dictionary lookup's
     * question; feed the range to [rangeRects] or a selection.
     */
    fun wordAt(x: Float, y: Float): Pair<Int, Int>? {
        val packed = Native.wordAt(handle, x, y)
        if (packed < 0) return null
        return (packed ushr 32).toInt() to (packed and 0xffffffffL).toInt()
    }

    /** Page-space rects covering a locator range on the current page. */
    fun rangeRects(start: Int, end: Int): List<android.graphics.RectF> {
        val flat = Native.rangeRects(handle, start, end)
        return (flat.indices step 4).map { i ->
            android.graphics.RectF(
                flat[i], flat[i + 1], flat[i] + flat[i + 2], flat[i + 1] + flat[i + 3])
        }
    }

    /**
     * The library row this session's book was imported into, or null for
     * a book that never reached one — an OPDS stream, or a session opened
     * without a library directory.
     *
     * The join between the reading view and the shelf: opening a book is
     * what adds it, so this is how an app learns which [Book] it just
     * created and can put it in a collection, mark it, or find it again.
     */
    fun bookId(): Long? = Native.sessionBookId(handle).takeIf { it != 0L }

    override fun close() {
        if (handle != 0L) {
            Native.close(handle)
            handle = 0L
        }
    }
}
