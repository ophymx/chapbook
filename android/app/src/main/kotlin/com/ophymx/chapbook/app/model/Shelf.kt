package com.ophymx.chapbook.app.model

import com.ophymx.chapbook.Book
import com.ophymx.chapbook.BookQuery
import com.ophymx.chapbook.Library
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.withContext
import java.io.File
import java.util.concurrent.Executors

/**
 * The library, on the one thread it is allowed to be on.
 *
 * A [Library] is a SQLite connection: movable, never shared. Every call
 * here hops to a dedicated thread and back, so a ViewModel can ask from
 * a coroutine without knowing that. The connection opens on first use,
 * on that thread, and stays open for the life of the process — the
 * database is WAL, so a reading session holding its own connection
 * beside this one is the ordinary arrangement.
 */
class Shelf(private val dir: File) {
    private val thread = Executors.newSingleThreadExecutor { r -> Thread(r, "chapbook-shelf") }
        .asCoroutineDispatcher()

    private val library: Library by lazy {
        checkNotNull(Library.open(dir.absolutePath)) { "the library at $dir did not open; see logcat" }
    }

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
    suspend fun importFile(path: String): Long? =
        withContext(thread) { library.importFile(path) }

    /** Record where a book syncs — the two services off the catalog entry it came from. */
    suspend fun setSyncTargets(book: Long, progressionUrl: String?, annotationContainer: String?): Boolean =
        withContext(thread) { library.setSyncTargets(book, progressionUrl, annotationContainer) }

    suspend fun syncProgressionUrl(book: Long): String? =
        withContext(thread) { library.syncProgressionUrl(book) }
}
