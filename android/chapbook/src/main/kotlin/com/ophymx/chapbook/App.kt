package com.ophymx.chapbook

import android.os.ParcelFileDescriptor

/**
 * The app's secret store, asked by key.
 *
 * The engine derives the key — [App.credentialKey] gives the same one
 * for the app's own lookups — and never parses what is stored under it:
 * a complete `Authorization` value, whatever the catalog took. Calls
 * arrive on whichever thread the layer is on, the catalog's or the sync
 * driver's, and must not prompt: a lookup is a lookup.
 */
interface CredentialStore {
    /** The stored value, or null for nothing stored. */
    fun get(key: String): String?

    fun set(key: String, value: String)

    fun forget(key: String)
}

/** What the reader's progress readout says, beside the whole-book bar. */
enum class ProgressLabel {
    /** "34%" of the whole book. */
    PERCENT,

    /** "6 left in chapter" — how many pages remain in this unit. */
    PAGES_LEFT,

    /** "unit 6/20 · page 2/11" — the raw indices. */
    CHAPTER_PAGE,
}

/**
 * Where the reader is, for the chrome, as one value read after a draw —
 * which is when a position is authoritative, because a restored position
 * lands on the first frame rather than at open.
 */
data class Place(
    val spine: Int,
    val spineLen: Int,
    val page: Int,
    val pageCount: Int,
    /** Whole-book progress, 0..1, spine-weighted like the engine's own progression. */
    val bookFraction: Double,
    /** Pages after this one in the current unit. */
    val pagesLeft: Int,
    /** Whether the engine's Back has anywhere to go — what decides whether *Return* is drawn. */
    val canGoBack: Boolean,
)

/** What opening a shelf row came to. */
sealed class Opened {
    /** The library's own copy, open. */
    data class Session(val session: com.ophymx.chapbook.Session) : Opened()

    /**
     * The platform's file: read the grant with [App.grant] by the row's
     * fingerprint, resolve it, and open with [App.openFd].
     */
    data object Adopted : Opened()

    /** Out of reach: the copy is gone, or an adopted book whose grant was never kept. */
    data object Missing : Opened()

    /** The row left the shelf, or the book would not open; logcat says why. */
    data object Failed : Opened()
}

/** How a platform transfer's HTTP status is read. */
enum class DownloadOutcome {
    /** The file is the book. Land it with [App.landDownload]. */
    LANDED,

    /** 401 or 403: worth a sign-in, not a retry with the same credential. */
    REFUSED,

    /** 404, 410, or any other client-side answer: retrying will not change its mind. */
    GONE,

    /** A 5xx, or no response at all: retry with backoff. */
    AGAIN,
}

/**
 * The application: what the app module used to write for itself in
 * Kotlin, written once in the engine and reached from here.
 *
 * Custody (which door a file comes in through, and how the book is found
 * again), the reader's place and memory rules, the search walk, saved
 * catalogs, browsing a catalog through its login, what a landed download
 * does, and sync credentialed per book from the app's [CredentialStore].
 * The platform stays the app's: the Keystore behind the store, the HTTP
 * stack behind the transport, `WorkManager` for the transfer, the memory
 * class to hand [Reader.cacheBudgetFor]. Answers come back as codes and
 * numbers; the app has the strings.
 *
 * Holds a library connection, so it is one thread's at a time and may
 * move between them — keep it on the thread the shelf is on — and it
 * coexists with a [Library], sessions and catalogs over the same
 * directory. [close] joins the sync driver if one was started.
 */
class App private constructor(private var handle: Long) : AutoCloseable {

    companion object {
        /**
         * Open the application over [libraryDir] — `context.filesDir` —
         * with the platform's fonts, the app's store and its transport.
         * [deviceName] is what a progression service shows beside this
         * device's position. Null if the library will not open.
         */
        fun open(
            libraryDir: String,
            credentials: CredentialStore,
            transport: SyncTransport,
            deviceName: String,
        ): App? {
            val handle = Native.appOpen(libraryDir, credentials, transport, deviceName)
            return if (handle == 0L) null else App(handle)
        }

        /**
         * The key a credential for [url] lives under — scheme, host and
         * any explicit port, never the path, which may itself be a
         * secret. What the app's own code asks its store for, so it
         * agrees with what the layer stored. Null for anything that is
         * not a URL with an origin.
         */
        fun credentialKey(url: String): String? = Native.credentialKey(url)

        /** The `Authorization` value for HTTP Basic. */
        fun basicAuthorization(username: String, password: String): String =
            Native.basicAuthorization(username, password)

        /** Read a transfer's status the way every front end reads it. */
        fun downloadOutcome(status: Int): DownloadOutcome =
            DownloadOutcome.entries.getOrElse(Native.downloadOutcome(status)) { DownloadOutcome.AGAIN }
    }

    // ---- Custody ----

    /** Import: copy a file into the library and answer with its row, or null. Blocking. */
    fun import(path: String): Long? = Native.appImport(handle, path).takeIf { it > 0 }

    /**
     * Adopt: record a book the platform owns and remember [grant] — the
     * persisted `content://` URI, as bytes — as the way to reach it
     * again. The library keeps no copy. Takes ownership of [pfd]. The
     * same bytes adopted twice are one row. Null when the descriptor is
     * not a book. Blocking.
     */
    fun adoptFd(pfd: ParcelFileDescriptor, grant: ByteArray): Long? =
        Native.appAdoptFd(handle, pfd.detachFd(), grant).takeIf { it > 0 }

    /** [adoptFd] for a session already open over a descriptor from [openFd]. */
    fun adopt(session: Session, grant: ByteArray): Long? =
        Native.appAdopt(handle, session.handle, grant).takeIf { it > 0 }

    /** The grant that reaches an adopted book, by the row's fingerprint, or null. */
    fun grant(fingerprint: String): ByteArray? = Native.appGrant(handle, fingerprint)

    fun rememberGrant(fingerprint: String, grant: ByteArray): Boolean =
        Native.appRememberGrant(handle, fingerprint, grant)

    fun forgetGrant(fingerprint: String): Boolean = Native.appForgetGrant(handle, fingerprint)

    /** Open a shelf row for reading, whichever door it came in through. Blocking. */
    fun openBook(book: Long): Opened {
        val values = Native.appOpenBook(handle, book)
        if (values.size < 2) return Opened.Failed
        return when (values[0]) {
            0L -> if (values[1] != 0L) Opened.Session(Session(values[1])) else Opened.Failed
            1L -> Opened.Adopted
            else -> Opened.Missing
        }
    }

    /**
     * A session over a descriptor with the app's own configuration — how
     * an adopted book is read once its grant has been resolved. Takes
     * ownership of [pfd]. Blocking.
     */
    fun openFd(pfd: ParcelFileDescriptor): Session? {
        val session = Native.appOpenFd(handle, pfd.detachFd())
        return if (session == 0L) null else Session(session)
    }

    // ---- Preferences ----

    /** The reader's chosen readout, kept for every launch. */
    var progressLabel: ProgressLabel
        get() = ProgressLabel.entries.getOrElse(Native.appProgressLabel(handle)) { ProgressLabel.PERCENT }
        set(value) {
            Native.appSetProgressLabel(handle, value.ordinal)
        }

    // ---- Catalogs ----

    /** A catalog the reader added: where it is and what it called itself. */
    data class CatalogRecord(val id: Long, val title: String, val url: String)

    /** The catalogs the reader has added, in the order they were added. */
    fun catalogs(): List<CatalogRecord> = Native.appCatalogIds(handle).mapNotNull { catalog(it) }

    /** One saved catalog, or null once removed. */
    fun catalog(id: Long): CatalogRecord? {
        val url = Native.appCatalogText(handle, id, 1) ?: return null
        return CatalogRecord(id, Native.appCatalogText(handle, id, 0) ?: "", url)
    }

    /** Add a catalog; [title] may be blank until its feed says what it is called. */
    fun addCatalog(url: String, title: String = ""): CatalogRecord? =
        Native.appAddCatalog(handle, url, title).takeIf { it > 0 }?.let { catalog(it) }

    fun renameCatalog(id: Long, title: String): Boolean = Native.appRenameCatalog(handle, id, title)

    /** Take a catalog off the list. Its books stay, and so does its credential. */
    fun removeCatalog(id: Long): Boolean = Native.appRemoveCatalog(handle, id)

    /**
     * Browse a saved catalog — or, with [id] 0, no row, a pasted URL — as
     * a [Catalog] over the app's transport and store, titled after the
     * saved row until its feed says otherwise. The handle is the
     * caller's to close, belongs to whichever thread does the blocking
     * fetches, and does not need this app to stay open.
     */
    fun browse(id: Long = 0): Catalog? {
        val catalog = Native.appBrowse(handle, id)
        return if (catalog == 0L) null else Catalog(catalog)
    }

    // ---- Downloads ----

    /**
     * Everything a landed download does: import the file the transfer
     * produced and record the sync services the entry advertised, read
     * before the transfer. The file is the caller's and is left where it
     * was; the same bytes twice are one row, so a retried job needs no
     * bookkeeping. The library row, or null. Blocking.
     */
    fun landDownload(path: String, progressionUrl: String?, annotationContainer: String?): Long? =
        Native.appLandDownload(handle, path, progressionUrl, annotationContainer).takeIf { it > 0 }

    // ---- Sync ----

    /** What [syncAll] came to. */
    enum class SyncStart { STARTED, NOTHING_TO_SYNC, FAILED }

    /**
     * Ask for every book with a service to reconcile, starting the app's
     * driver on first use. [waker] runs on the driver's thread once per
     * report and must only post to the main thread to come [drainSync];
     * the first one given is the one kept.
     */
    fun syncAll(waker: Runnable?): SyncStart = when (Native.appSyncAll(handle, waker)) {
        1 -> SyncStart.STARTED
        0 -> SyncStart.NOTHING_TO_SYNC
        else -> SyncStart.FAILED
    }

    fun syncBook(book: Long, waker: Runnable?): Boolean = Native.appSyncBook(handle, book, waker)

    /** Everything sync reported since the last drain, oldest first. */
    fun drainSync(): List<SyncReport> {
        val out = mutableListOf<SyncReport>()
        while (true) {
            val values = Native.appSyncNext(handle)
            if (values.isEmpty()) break
            out.add(syncReport(values, Native.appSyncDetail(handle), Native.appSyncMarksError(handle)))
        }
        return out
    }

    override fun close() {
        if (handle != 0L) {
            Native.appClose(handle)
            handle = 0
        }
    }
}

/**
 * The reader's policy over a session: the decisions a reading screen
 * makes that are not about drawing, each one written in the engine
 * rather than once per app.
 */
object Reader {
    /** Where the reader is, read after a draw. Lays the current unit out if nothing has yet. */
    fun place(session: Session): Place {
        val v = Native.readerPlace(session.handle)
        if (v.size < 7) return Place(0, 0, 0, 0, 0.0, 0, false)
        return Place(v[0].toInt(), v[1].toInt(), v[2].toInt(), v[3].toInt(), v[4], v[5].toInt(), v[6] != 0.0)
    }

    /**
     * How much of what the platform says this process may use goes to a
     * session's cache: a quarter of [availableBytes], between a floor and
     * a cap. Hand `ActivityManager.memoryClass` in bytes.
     */
    fun cacheBudgetFor(availableBytes: Long): Long = Native.readerCacheBudgetFor(availableBytes)

    /**
     * What a memory warning does: halve the budget, which evicts at
     * once, and release the caches. The budget now in force.
     */
    fun afterMemoryWarning(session: Session): Long = Native.readerAfterMemoryWarning(session.handle)

    /** Jump to a hit and leave it selected. Whether the position moved. */
    fun showHit(session: Session, hit: SearchHit): Boolean =
        Native.readerShowHit(session.handle, hit.spine, hit.start, hit.end)

    /** The selection becomes a highlight, and the selection goes. Null with nothing selected. */
    fun highlightSelection(session: Session): Long? =
        Native.readerHighlightSelection(session.handle).takeIf { it > 0 }

    /** The selection becomes a note, and the selection goes. Null with nothing selected. */
    fun noteOnSelection(session: Session, body: String): Long? =
        Native.readerNoteOnSelection(session.handle, body).takeIf { it > 0 }
}

/**
 * A search walked one unit at a time on the session's thread, so the
 * page stays responsive between steps: call [step] from a coroutine on
 * the main dispatcher, `yield()` between units, and read [hits] after
 * each. It stops itself at a cap past which a results list is a scroll
 * nobody finishes. Null from [open] means the query was blank, which
 * clears rather than searches.
 */
class SearchWalk private constructor(private var handle: Long) : AutoCloseable {
    companion object {
        fun open(query: String): SearchWalk? {
            val handle = Native.searchWalkOpen(query)
            return if (handle == 0L) null else SearchWalk(handle)
        }
    }

    /** Search the next unit. Whether there is another to search. */
    fun step(session: Session): Boolean = Native.searchWalkStep(handle, session.handle)

    /** Every hit so far, in reading order. */
    fun hits(): List<SearchHit> {
        val count = Native.searchWalkHitCount(handle)
        if (count <= 0) return emptyList()
        return (0 until count).mapNotNull { index ->
            val values = Native.searchWalkHit(handle, index)
            if (values.size < 5) return@mapNotNull null
            SearchHit(
                spine = values[0].toInt(),
                start = values[1].toInt(),
                end = values[2].toInt(),
                context = Native.searchWalkContext(handle, index) ?: "",
                matchStart = values[3].toInt(),
                matchEnd = values[4].toInt(),
            )
        }
    }

    override fun close() {
        if (handle != 0L) {
            Native.searchWalkClose(handle)
            handle = 0
        }
    }
}
