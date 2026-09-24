package com.ophymx.chapbook.app.model

import com.ophymx.chapbook.Catalog
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.withContext
import java.util.concurrent.Executors

/**
 * One catalog being browsed, on the one thread it is allowed to be on.
 *
 * Every [Catalog] call blocks and the handle is one thread's at a time,
 * so a session owns a thread and hops every call onto it. The catalog is
 * opened on first use — through the app, so it browses over the app's
 * transport and signs in through the app's credential store — and
 * closed on the thread at the end. [savedId] is 0 for a catalog that
 * has no saved row, a pasted URL.
 */
class CatalogSession(private val shelf: Shelf, private val savedId: Long) : AutoCloseable {
    private val executor = Executors.newSingleThreadExecutor { r -> Thread(r, "chapbook-catalog") }
    private val thread = executor.asCoroutineDispatcher()
    private var catalog: Catalog? = null

    suspend fun <T> use(block: Catalog.() -> T): T = withContext(thread) {
        val c = catalog
            ?: checkNotNull(shelf.withApp { browse(savedId) }) { "the catalog did not open; see logcat" }
                .also { catalog = it }
        c.block()
    }

    override fun close() {
        executor.execute {
            catalog?.close()
            catalog = null
        }
        executor.shutdown()
    }
}
