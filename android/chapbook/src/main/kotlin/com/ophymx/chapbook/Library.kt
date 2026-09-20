package com.ophymx.chapbook

/** How far through a book the reader is, as a shelf groups it. */
enum class ReadingState(internal val code: Int) {
    /** Never opened. */
    Unread(1),

    /** Opened, not finished — what a "continue reading" row wants. */
    Reading(2),

    /** Reached the end at least once, whatever the position says now. */
    Finished(3);

    internal companion object {
        fun of(code: Long) = entries.firstOrNull { it.code.toLong() == code } ?: Unread
    }
}

/** How a query orders its rows. */
enum class Sort(internal val code: Int) {
    /** Newest addition first: a stable listing. */
    Added(0),

    /** The most recent thing that happened, read or added — what a shelf shows first. */
    Read(1),

    Title(2),

    /** First author, then title. A book with no author sorts last. */
    Author(3),

    /** Series, then position within it. Books in no series sort last. */
    Series(4),
}

/**
 * What to list, and in what order. The defaults are the whole shelf.
 *
 * [search] matches whole words by prefix over title, authors and series,
 * folded for case *and* accents — "bronte" finds Brontë. Text with
 * nothing searchable in it (punctuation alone) matches no book rather
 * than every book, so a filter that came back empty is not one that was
 * ignored.
 */
data class BookQuery(
    val search: String? = null,
    val collection: Long? = null,
    val series: String? = null,
    val state: ReadingState? = null,
    val sort: Sort = Sort.Added,
    val limit: Int? = null,
    val offset: Int = 0,
)

/** A collection a book is in, as a row names them. */
data class CollectionRef(val id: Long, val name: String)

/** A collection as a list of collections shows them. */
data class Collection(val id: Long, val name: String, val bookCount: Int)

/**
 * One book on the shelf.
 *
 * Timestamps are Unix seconds and null means *never*, not 1970.
 *
 * [progress] is deliberately not what [state] is derived from: a book
 * skimmed to the last page reads 1.0 without being finished, and a
 * finished book reopened reads near 0 without being unread. Draw the
 * bar from one and the badge from the other.
 */
data class Book(
    val id: Long,
    val title: String,
    val authors: List<String>,
    val series: String?,
    val seriesIndex: Float?,
    val collections: List<CollectionRef>,
    val state: ReadingState,
    val progress: Float?,
    val addedAt: Long,
    val lastRead: Long?,
    val finishedAt: Long?,
    /** SHA-1 of the file's bytes, hex — the key to map a URI grant to. */
    val fingerprint: String,
    /** The library's own copy, or null for a book it holds no copy of. */
    val filePath: String?,
    /** Kept at import, so a shelf need not reopen every book to draw one. */
    val coverPath: String?,
)

/**
 * The shelf: the books this app has opened, and the groupings over them.
 *
 * Held open for as long as a shelf is on screen and [close]d exactly
 * once. Like [Session] it is movable between threads and must never be
 * touched from two at the same time — SQLite's connection underneath is
 * one thread's at a time — but it may be held *while* a session is open:
 * the database is WAL, and two connections is the ordinary way to read a
 * shelf while a book is being read.
 *
 * A book reaches the library by being *opened*, not by being imported
 * here: [Session.open] and [Session.openFd] record it, and
 * [Session.bookId] says which row that became. So an app's "add to
 * library" is a read, and this class is what browses the result.
 */
class Library private constructor(private var handle: Long) : AutoCloseable {

    companion object {
        /**
         * Open (creating if needed) the library at [dir].
         *
         * [dir] should be the same `context.filesDir` a session is given —
         * a session that wrote its position somewhere else is a session
         * whose book this shelf will not list. Returns null if it will not
         * open; logcat says why.
         */
        fun open(dir: String): Library? {
            val handle = Native.libraryOpen(dir)
            return if (handle == 0L) null else Library(handle)
        }
    }

    /**
     * Run a query and read its rows.
     *
     * The rows are copied out here rather than left behind a cursor,
     * which is the point: a search box issues a query on every keystroke
     * and the list being drawn must not move underneath the draw.
     */
    fun books(query: BookQuery = BookQuery()): List<Book> {
        val shelf = Native.libraryQuery(
            handle,
            query.search.orEmpty(),
            query.series.orEmpty(),
            query.collection ?: 0L,
            query.state?.code ?: 0,
            query.sort.code,
            query.limit ?: 0,
            query.offset,
        )
        if (shelf == 0L) return emptyList()
        try {
            val count = Native.shelfLen(shelf)
            if (count <= 0) return emptyList()
            return (0 until count).map { index -> read(shelf, index) }
        } finally {
            Native.shelfFree(shelf)
        }
    }

    private fun read(shelf: Long, index: Int): Book {
        // One crossing for the seven numbers rather than seven: a shelf of
        // a hundred books would otherwise make seven hundred JNI calls to
        // draw once.
        val fields = Native.shelfBook(shelf, index)
        val fractions = Native.shelfBookFractions(shelf, index)
        val authorCount = fields[5].toInt()
        val collectionCount = fields[6].toInt()
        return Book(
            id = fields[0],
            title = Native.shelfTitle(shelf, index),
            authors = (0 until authorCount).map { Native.shelfAuthor(shelf, index, it) },
            series = Native.shelfSeries(shelf, index).ifEmpty { null },
            seriesIndex = fractions[1].takeIf { it >= 0f },
            collections = (0 until collectionCount).map {
                CollectionRef(
                    Native.shelfCollectionId(shelf, index, it),
                    Native.shelfCollectionName(shelf, index, it),
                )
            },
            state = ReadingState.of(fields[4]),
            progress = fractions[0].takeIf { it >= 0f },
            addedAt = fields[1],
            lastRead = fields[2].takeIf { it != 0L },
            finishedAt = fields[3].takeIf { it != 0L },
            fingerprint = Native.shelfFingerprint(shelf, index),
            filePath = Native.shelfFilePath(shelf, index).ifEmpty { null },
            coverPath = Native.shelfCoverPath(shelf, index).ifEmpty { null },
        )
    }

    /** Every collection, with its size, oldest first. */
    fun collections(): List<Collection> {
        val flat = Native.libraryCollections(handle)
        return (flat.indices step 2).map { i ->
            Collection(
                id = flat[i],
                name = Native.libraryCollectionName(handle, flat[i]),
                bookCount = flat[i + 1].toInt(),
            )
        }
    }

    /**
     * Make a collection, or return the one that already has this name.
     * Idempotent, so adding a book to "Sci-Fi" need not ask first. 0 on
     * failure.
     */
    fun createCollection(name: String): Long = Native.libraryCreateCollection(handle, name)

    fun renameCollection(collection: Long, name: String): Boolean =
        Native.libraryRenameCollection(handle, collection, name)

    /** The books stay; only the grouping goes, and the name frees up. */
    fun deleteCollection(collection: Long): Boolean =
        Native.libraryDeleteCollection(handle, collection)

    fun addToCollection(book: Long, collection: Long): Boolean =
        Native.libraryAddToCollection(handle, book, collection)

    fun removeFromCollection(book: Long, collection: Long): Boolean =
        Native.libraryRemoveFromCollection(handle, book, collection)

    /**
     * Take a book off the shelf. Soft: the row keeps its id, its position
     * and its annotations, so opening the same file again is the same
     * book with its marks intact.
     */
    fun deleteBook(book: Long): Boolean = Native.libraryDeleteBook(handle, book)

    /**
     * Mark a book finished, or take the mark back.
     *
     * A session records this itself when the reader reaches the end, so
     * this is the other direction — the "mark as read" a reader taps for
     * a book they finished elsewhere. Marking twice keeps the first
     * timestamp.
     */
    /**
     * Put a file on the shelf, answering with its row or null.
     *
     * **Where a download your app ran itself comes back.** Take a
     * [DownloadRequest] off a catalog entry, fetch it with `WorkManager`,
     * `DownloadManager` or a worker over OkHttp, and hand the finished
     * file here. The format is read from the bytes, so whatever the
     * platform named the file is fine — a `content://` copy, a cache file
     * under a generated name.
     *
     * The file is not consumed: the library copies what it imports and
     * never deletes the source, which is yours. Importing the same bytes
     * twice answers with the same row rather than shelving a duplicate,
     * which is what makes a retried worker safe — `WorkManager` reruns
     * one after a crash or a lost network, and that needs no coordination
     * with this call.
     *
     * Sync services are not in the file. They live in the catalog entry,
     * so pass [DownloadRequest.progressionUrl] and
     * [DownloadRequest.annotationContainer] — captured *before* the
     * transfer, while the feed was still open — to [setSyncTargets] once
     * this returns a row.
     *
     * Blocking: it copies a whole book. Keep it off the main thread.
     */
    fun importFile(path: String): Long? =
        Native.libraryImportFile(handle, path).takeIf { it > 0 }

    /**
     * Record where a book syncs — the two service URLs off the catalog
     * entry it was downloaded from. Null holds none; two nulls make it
     * local again. Both URLs are opaque and may embed a per-user key:
     * never log them, and key any credential by origin, not by URL.
     */
    fun setSyncTargets(book: Long, progressionUrl: String?, annotationContainer: String?): Boolean =
        Native.librarySetSyncTargets(handle, book, progressionUrl, annotationContainer)

    /** The progression service this book syncs its position to, or null. */
    fun syncProgressionUrl(book: Long): String? =
        Native.librarySyncProgressionUrl(handle, book)

    /** The annotation container this book syncs its marks with, or null. */
    fun syncAnnotationContainer(book: Long): String? =
        Native.librarySyncAnnotationContainer(handle, book)

    fun setFinished(book: Long, finished: Boolean): Boolean =
        Native.librarySetFinished(handle, book, finished)

    override fun close() {
        if (handle != 0L) {
            Native.libraryClose(handle)
            handle = 0L
        }
    }
}
