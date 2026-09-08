package com.ophymx.chapbook

/**
 * The raw JNI surface. Nothing outside this package should call it —
 * [Session] is the type worth holding.
 *
 * This mirrors `crates/chapbook-jni/src/android.rs` by name. It is a spike:
 * when the C ABI exists these become calls into it rather than into
 * `chapbook-reader` directly.
 *
 * Nothing here references the Rust side at compile time and nothing there
 * references this, so a renamed function is an `UnsatisfiedLinkError` on
 * first call rather than a build failure. `android/build-jni.sh` compares
 * these declarations against the `.so`'s exported symbols for that reason.
 */
internal object Native {
    init {
        System.loadLibrary("chapbook_jni")
    }

    external fun initLogging(verbose: Boolean)

    external fun open(path: String, libraryDir: String): Long

    /** Takes ownership of [fd]; the caller must have detached it. */
    external fun openFd(fd: Int, libraryDir: String): Long

    external fun close(handle: Long)
    external fun fontReport(handle: Long): String

    /** Named for Kotlin's sake: `suspend` is a modifier here. */
    external fun suspendSession(handle: Long)

    external fun releaseCaches(handle: Long)
    external fun cacheBytes(handle: Long): Long
    external fun cacheBudget(handle: Long): Long
    external fun setMetrics(handle: Long, width: Float, height: Float, margin: Float, scale: Float)
    external fun nextPage(handle: Long): Boolean
    external fun prevPage(handle: Long): Boolean
    external fun cycleTheme(handle: Long)
    external fun setWaker(handle: Long, waker: Runnable?)
    external fun pollLoaded(handle: Long): Boolean
    external fun hasPendingLoads(handle: Long): Boolean
    external fun nextEvent(handle: Long): Long
    external fun eventMessage(handle: Long): String?
    external fun spineLen(handle: Long): Int
    external fun pageCount(handle: Long): Int
    external fun bookKind(handle: Long): Int
    external fun settings(handle: Long): FloatArray
    external fun setSettings(
        handle: Long,
        baseFontPx: Float,
        lineHeight: Float,
        justify: Boolean,
        publisherStyles: Boolean,
        theme: Int,
        thisBook: Boolean,
    )
    external fun fontFamily(handle: Long): String?
    external fun setFontFamily(handle: Long, family: String?, thisBook: Boolean)
    external fun fontFamilies(handle: Long): Array<String>
    external fun selectionBegin(handle: Long, x: Float, y: Float): Boolean
    external fun selectionDrag(handle: Long, x: Float, y: Float)
    external fun selectWordAt(handle: Long, x: Float, y: Float): Boolean
    external fun selectRange(handle: Long, start: Int, end: Int)
    external fun selectionClear(handle: Long)
    external fun selectedRange(handle: Long): Long
    external fun selectedText(handle: Long): String?
    external fun linkAt(handle: Long, x: Float, y: Float): String?
    external fun followLink(handle: Long, href: String): Boolean
    external fun addHighlight(handle: Long): Long
    external fun addNote(handle: Long, body: String): Long
    external fun addBookmark(handle: Long): Long
    external fun highlightAt(handle: Long, x: Float, y: Float): Long
    external fun setHighlightColor(handle: Long, id: Long, color: String?)
    external fun removeAnnotation(handle: Long, id: Long)
    external fun gotoAnnotation(handle: Long, id: Long): Boolean
    external fun annotationCount(handle: Long): Int
    external fun annotation(handle: Long, index: Int): LongArray
    external fun annotationText(handle: Long, index: Int): String?
    external fun annotationColor(handle: Long, index: Int): String?
    external fun setPageZoom(handle: Long, zoom: Float, focusX: Float, focusY: Float): Boolean
    external fun panPage(handle: Long, dx: Float, dy: Float): Boolean
    external fun pageZoom(handle: Long): Float
    external fun pagePan(handle: Long): FloatArray
    external fun toc(handle: Long): LongArray
    external fun tocLabel(handle: Long, index: Int): String?
    external fun gotoToc(handle: Long, index: Int): Boolean
    external fun search(handle: Long, query: String, limit: Int): Int
    external fun searchUnit(handle: Long, spine: Int, query: String): Int
    external fun searchHit(handle: Long, index: Int): LongArray
    external fun searchContext(handle: Long, index: Int): String?
    external fun locator(handle: Long): Long
    external fun gotoLocator(handle: Long, spine: Int, offset: Int): Boolean
    external fun gotoAnchor(handle: Long, spine: Int, fragment: String): Boolean
    external fun canGoBack(handle: Long): Boolean
    external fun clearBookSettings(handle: Long)
    external fun syncOpen(
        libraryDir: String,
        deviceId: String,
        deviceName: String,
        transport: SyncTransport,
        waker: Runnable?,
    ): Long
    external fun syncRequestAll(handle: Long): Boolean
    external fun syncRequestBook(handle: Long, book: Long): Boolean
    external fun syncNext(handle: Long): LongArray
    external fun syncDetail(handle: Long): String?
    external fun syncMarksError(handle: Long): String?
    external fun syncClose(handle: Long)
    external fun librarySetSyncTargets(
        handle: Long,
        book: Long,
        progressionUrl: String?,
        annotationContainer: String?,
    ): Boolean
    external fun librarySyncProgressionUrl(handle: Long, book: Long): String?
    external fun librarySyncAnnotationContainer(handle: Long, book: Long): String?
    external fun catalogOpen(transport: SyncTransport): Long
    external fun catalogClose(handle: Long)
    external fun catalogSetAuthorization(handle: Long, value: String?)
    external fun catalogSetBasicAuth(handle: Long, username: String, password: String)
    external fun catalogFetch(handle: Long, url: String): Int
    external fun catalogSearch(handle: Long, query: String): Int
    external fun catalogFeedTitle(handle: Long): String?
    external fun catalogEntryCount(handle: Long): Int
    external fun catalogEntry(handle: Long, index: Int): LongArray
    external fun catalogEntryText(handle: Long, index: Int, field: Int): String?
    external fun catalogEntryAuthor(handle: Long, index: Int, author: Int): String?
    external fun catalogPageHref(handle: Long, direction: Int): String?
    external fun catalogHasSearch(handle: Long): Boolean
    external fun catalogDownload(handle: Long, index: Int, libraryDir: String): Long
    external fun catalogAuthTitle(handle: Long): String?
    external fun catalogAuthOffersBasic(handle: Long): Boolean
    external fun position(handle: Long): Long
    external fun title(handle: Long): String
    external fun renderSize(handle: Long): Long
    external fun renderInto(handle: Long, bitmap: android.graphics.Bitmap): Int
    external fun conformance(path: String, libraryDir: String): String

    // Input. Actions cross as their engine names — "next-page" and so on —
    // rather than as ordinals, because `Action` is non-exhaustive on the
    // Rust side and nothing here would notice it being reordered. The empty
    // string is "no action". See `chapbook_core::input`.

    /** `"ltr"` or `"rtl"`, off the book. */
    external fun readingDirection(handle: Long): String

    /** `middle` is an action name, or `""` for a band that does nothing. */
    external fun setTapZones(handle: Long, prevFraction: Float, nextFraction: Float, middle: String)

    /** Logical units — view pixels over density — in panel space. */
    external fun tapAction(handle: Long, x: Float, y: Float): String

    /** Takes an `android.view.KeyEvent.KEYCODE_*` value. */
    external fun actionForKeyCode(handle: Long, keyCode: Int): String

    /** 0 changed, 1 unchanged, 2 not the engine's, -1 unusable. */
    external fun applyAction(handle: Long, action: String): Int

    // The text surface: the page's text with geometry, for accessibility
    // trees, TTS word highlighting, and dictionary lookup. Ranges pack
    // like `position` — `start shl 32 or end` — and tables flatten into
    // primitive arrays so TTS reads a page in one crossing.

    /** Runs on the current page; -1 until laid out, 0 for a comic. */
    external fun pageTextRunCount(handle: Long): Int

    /** Locator range packed `start shl 32 or end`; -1 on a bad index. */
    external fun pageTextRunRange(handle: Long, index: Int): Long

    /** Page-space `[x, y, w, h]`; empty on a bad index. */
    external fun pageTextRunRect(handle: Long, index: Int): FloatArray

    external fun pageTextRunText(handle: Long, index: Int): String

    /** The page as one speakable string for TTS; `""` until laid out. */
    external fun speakableText(handle: Long): String

    /**
     * Four ints per word: textStart, textEnd (char offsets into
     * [speakableText]), locatorStart, locatorEnd.
     */
    external fun pageWords(handle: Long): IntArray

    /** Word under a panel point, packed like a range; -1 for none. */
    external fun wordAt(handle: Long, x: Float, y: Float): Long

    /** Four floats per rect covering a locator range on this page. */
    external fun rangeRects(handle: Long, start: Int, end: Int): FloatArray

    // The shelf. Two handles, matching the C ABI's: a library connection
    // and one query's rows held still. The query crosses flattened,
    // because a struct crossing JNI is a Java class the Rust side would
    // have to name by signature — a link error nothing checks.

    /** 0 on failure; an empty [dir] asks for the platform default, which Android has none of. */
    external fun libraryOpen(dir: String): Long

    external fun libraryClose(handle: Long)

    /**
     * 0 on failure. Empty strings and zeros mean "do not narrow", so the
     * all-defaults call is the whole shelf.
     */
    external fun libraryQuery(
        handle: Long,
        search: String,
        series: String,
        collection: Long,
        state: Int,
        sort: Int,
        limit: Int,
        offset: Int,
    ): Long

    external fun shelfFree(handle: Long)

    /** `-1` for a bad handle, so "empty" and "broken" differ. */
    external fun shelfLen(handle: Long): Int

    /** `[id, addedAt, lastRead, finishedAt, state, authorCount, collectionCount]`. */
    external fun shelfBook(handle: Long, index: Int): LongArray

    /** `[progress, seriesIndex]`, each `-1` when absent. */
    external fun shelfBookFractions(handle: Long, index: Int): FloatArray

    external fun shelfTitle(handle: Long, index: Int): String
    external fun shelfAuthor(handle: Long, index: Int, author: Int): String
    external fun shelfSeries(handle: Long, index: Int): String
    external fun shelfFingerprint(handle: Long, index: Int): String
    external fun shelfFilePath(handle: Long, index: Int): String
    external fun shelfCoverPath(handle: Long, index: Int): String
    external fun shelfCollectionId(handle: Long, index: Int, which: Int): Long
    external fun shelfCollectionName(handle: Long, index: Int, which: Int): String

    /** `[id, bookCount, id, bookCount, ...]`, oldest first. */
    external fun libraryCollections(handle: Long): LongArray

    external fun libraryCollectionName(handle: Long, collection: Long): String
    external fun libraryCreateCollection(handle: Long, name: String): Long
    external fun libraryRenameCollection(handle: Long, collection: Long, name: String): Boolean
    external fun libraryDeleteCollection(handle: Long, collection: Long): Boolean
    external fun libraryAddToCollection(handle: Long, book: Long, collection: Long): Boolean
    external fun libraryRemoveFromCollection(handle: Long, book: Long, collection: Long): Boolean
    external fun libraryDeleteBook(handle: Long, book: Long): Boolean
    external fun librarySetFinished(handle: Long, book: Long, finished: Boolean): Boolean

    /** 0 for a book that never reached the library. */
    external fun sessionBookId(handle: Long): Long
}
