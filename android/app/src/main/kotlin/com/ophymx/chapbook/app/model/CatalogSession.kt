package com.ophymx.chapbook.app.model

import com.ophymx.chapbook.Catalog
import com.ophymx.chapbook.SyncTransport
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.withContext
import java.util.concurrent.Executors

/**
 * One open catalog, on the one thread it is allowed to be on.
 *
 * Every [Catalog] call blocks and the handle is one thread's at a time,
 * so a session owns a thread and hops every call onto it. The catalog is
 * opened on that thread on first use and closed on it at the end.
 */
class CatalogSession(private val transport: SyncTransport) : AutoCloseable {
    private val executor = Executors.newSingleThreadExecutor { r -> Thread(r, "chapbook-catalog") }
    private val thread = executor.asCoroutineDispatcher()
    private var catalog: Catalog? = null

    suspend fun <T> use(block: Catalog.() -> T): T = withContext(thread) {
        val c = catalog ?: Catalog(transport).also { catalog = it }
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
