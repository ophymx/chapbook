package com.ophymx.chapbook.app.model

import android.os.Build
import com.ophymx.chapbook.App
import com.ophymx.chapbook.Book
import com.ophymx.chapbook.BookQuery
import com.ophymx.chapbook.CredentialStore
import com.ophymx.chapbook.Library
import com.ophymx.chapbook.SyncTransport
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import java.io.File
import java.util.concurrent.Executors

/**
 * The library and the application, on the one thread they are allowed
 * to be on.
 *
 * A [Library] is a SQLite connection and an [App] holds one: movable,
 * never shared. Every call here hops to a dedicated thread and back, so
 * a ViewModel can ask from a coroutine without knowing that. Both open
 * on first use, on that thread, and stay open for the life of the
 * process — the database is WAL, so a reading session holding its own
 * connection beside these is the ordinary arrangement.
 */
class Shelf(
    private val dir: File,
    private val credentials: CredentialStore,
    private val transport: SyncTransport,
) {
    private val thread = Executors.newSingleThreadExecutor { r -> Thread(r, "chapbook-shelf") }
        .asCoroutineDispatcher()

    private val library: Library by lazy {
        checkNotNull(Library.open(dir.absolutePath)) { "the library at $dir did not open; see logcat" }
    }

    private val app: App by lazy {
        checkNotNull(App.open(dir.absolutePath, credentials, transport, Build.MODEL)) {
            "the app at $dir did not open; see logcat"
        }
    }

    /** Ask the application layer something, on its thread. */
    suspend fun <T> withApp(block: App.() -> T): T = withContext(thread) { app.block() }

    /**
     * [withApp] for a caller with no coroutine — an opener already on an
     * IO thread, a test. Never from the main thread: it blocks.
     */
    fun <T> blockingApp(block: App.() -> T): T = runBlocking { withApp(block) }

    suspend fun books(query: BookQuery = BookQuery()): List<Book> =
        withContext(thread) { library.books(query) }

    /** One row by id, or null once it is gone. The shelf is small enough to scan. */
    suspend fun book(id: Long): Book? =
        withContext(thread) { library.books(BookQuery()).firstOrNull { it.id == id } }

    suspend fun setFinished(book: Long, finished: Boolean): Boolean =
        withContext(thread) { library.setFinished(book, finished) }

    suspend fun remove(book: Long): Boolean =
        withContext(thread) { library.deleteBook(book) }

    /** Copy a file into the library. Blocking by nature; this hops off the caller's thread. */
    suspend fun importFile(path: String): Long? = withApp { import(path) }

    /**
     * What a landed download does: shelve the file and record where the
     * book syncs — the two services off the catalog entry it came from,
     * read before the transfer. The file stays the caller's.
     */
    suspend fun landDownload(path: String, progressionUrl: String?, annotationContainer: String?): Long? =
        withApp { landDownload(path, progressionUrl, annotationContainer) }

    suspend fun syncProgressionUrl(book: Long): String? =
        withContext(thread) { library.syncProgressionUrl(book) }
}
