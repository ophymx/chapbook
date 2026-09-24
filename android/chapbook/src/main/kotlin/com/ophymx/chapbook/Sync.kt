package com.ophymx.chapbook

/**
 * One HTTP answer, as the sync transport reports it.
 *
 * [headers] is names and values interleaved. Pass at least `ETag` and
 * `Location` when the server sent them: a Web Annotation container
 * carries its whole concurrency story in the first and says where it put
 * a new mark in the second, and a transport that drops them makes safe
 * concurrent editing impossible.
 */
class SyncResponse(
    @JvmField val status: Int,
    @JvmField val contentType: String?,
    @JvmField val headers: Array<String>,
    @JvmField val body: ByteArray,
)

/**
 * The app's networking, driving sync.
 *
 * This binding deliberately bundles no HTTP stack: a Rust TLS stack here
 * would trust its own roots and ignore the user's CAs, enterprise roots
 * and network security config, so requests go through the platform's own
 * client — `HttpURLConnection`, OkHttp, whatever the app already has.
 * Attach `Authorization` per request from whatever store the app keeps;
 * no credential ever crosses this interface.
 *
 * The rules are the engine's transport contract: 4xx and 5xx are
 * responses, not exceptions; follow redirects; send the given headers
 * (interleaved names and values) unaltered; do not retry. Throw only
 * when the request produced no response at all — the exception's message
 * reaches the sync report. **Calls arrive on the sync worker's thread**
 * and must block until the transfer settles.
 */
interface SyncTransport {
    fun get(url: String, headers: Array<String>): SyncResponse

    /** [method] is `"POST"`, `"PUT"` or `"DELETE"`; [body] may be empty. */
    fun send(method: String, url: String, headers: Array<String>, body: ByteArray): SyncResponse
}

/** What one drained sync report says. */
sealed class SyncReport {
    /**
     * A book reconciled. Failures inside it — an unreachable service, a
     * refused write — live in [position], [detail] and [marksError],
     * because one dead host must not read as a dead batch.
     */
    data class Book(
        val book: Long,
        val position: PositionOutcome,
        /** The refusal or failure explained, when there is one. */
        val detail: String?,
        val marksCreated: Long,
        val marksUpdated: Long,
        val marksDeleted: Long,
        val marksAdopted: Long,
        val marksRefreshed: Long,
        val marksMerged: Long,
        val marksConflicts: Long,
        /**
         * Another device's deletions arriving — taken off this shelf
         * because a complete container listing no longer holds them.
         * Distinct from [marksDeleted], this device's own deletions
         * reaching the container.
         */
        val marksWithdrawn: Long,
        /**
         * The container had more pages than one pass reads: the pull saw
         * a prefix and no deletion was inferred, so a mark another
         * device removed may still be sitting here. Changes what the
         * counts above mean, which is why it is worth showing.
         */
        val listingTruncated: Boolean,
        /** The container could not be reached, when it could not be. */
        val marksError: String?,
    ) : SyncReport()

    /** A book did not reconcile at all; the rest of the batch still ran. */
    data class BookFailed(val book: Long, val reason: String) : SyncReport()

    /** A batch finished — the signal to stop showing a spinner. */
    data class Finished(val books: Long) : SyncReport()

    /**
     * A batch could not start — the library would not answer. Only
     * [App.drainSync] reports it; a [SyncWorker] has its library by then.
     */
    data class Broken(val reason: String) : SyncReport()
}

/** What happened to a book's reading position. */
enum class PositionOutcome { IDLE, PUSHED, PULLED, REFUSED, CONFLICT, FAILED }

/**
 * One drained report from its flattened form — `[kind, book, position,
 * created, updated, deleted, adopted, refreshed, merged, conflicts,
 * books, withdrawn, truncated]` — and the two strings read beside it.
 * Shared by [SyncWorker.drain] and [App.drainSync], which flatten alike.
 */
internal fun syncReport(values: LongArray, detail: String?, marksError: String?): SyncReport =
    when (values[0]) {
        0L -> SyncReport.Book(
            book = values[1],
            position = PositionOutcome.entries
                .getOrElse(values[2].toInt()) { PositionOutcome.FAILED },
            detail = detail,
            marksCreated = values[3],
            marksUpdated = values[4],
            marksDeleted = values[5],
            marksAdopted = values[6],
            marksRefreshed = values[7],
            marksMerged = values[8],
            marksConflicts = values[9],
            marksWithdrawn = values[11],
            listingTruncated = values[12] != 0L,
            marksError = marksError,
        )
        1L -> SyncReport.BookFailed(values[1], detail ?: "")
        3L -> SyncReport.Broken(detail ?: "")
        else -> SyncReport.Finished(values[10])
    }

/**
 * A sync worker over one library, on its own thread.
 *
 * Ask with [requestAll] or [requestBook]; reports arrive through
 * [drain], one per book and then a [SyncReport.Finished]. The waker runs
 * on the worker thread once per queued report and must only nudge the
 * main thread — a `Handler.post` — to come drain. [close] joins the
 * thread; call it from something like `onStop`, knowing it blocks for
 * the book in flight.
 *
 * [deviceId] is how a progression service tells this device's positions
 * from another's: mint one once, store it, pass the same one forever.
 */
class SyncWorker(
    libraryDir: String,
    deviceId: String,
    deviceName: String,
    transport: SyncTransport,
    waker: Runnable? = null,
) : AutoCloseable {
    private var handle: Long =
        Native.syncOpen(libraryDir, deviceId, deviceName, transport, waker)

    init {
        check(handle != 0L) { "sync did not open; see logcat" }
    }

    fun requestAll(): Boolean = Native.syncRequestAll(handle)

    fun requestBook(book: Long): Boolean = Native.syncRequestBook(handle, book)

    /** Everything reported since the last drain, oldest first. */
    fun drain(): List<SyncReport> {
        val out = mutableListOf<SyncReport>()
        while (true) {
            val values = Native.syncNext(handle)
            if (values.isEmpty()) break
            out.add(syncReport(values, Native.syncDetail(handle), Native.syncMarksError(handle)))
        }
        return out
    }

    /** Joins the worker thread; blocks for the book in flight. */
    override fun close() {
        if (handle != 0L) {
            Native.syncClose(handle)
            handle = 0
        }
    }
}
