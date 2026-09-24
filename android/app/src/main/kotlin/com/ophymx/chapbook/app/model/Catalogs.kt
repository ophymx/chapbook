package com.ophymx.chapbook.app.model

import com.ophymx.chapbook.App
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

/** A catalog the reader added: where it is and what it called itself. */
data class SavedCatalog(val id: String, val title: String, val url: String)

/**
 * The catalogs the reader has added, in the order they were added, as
 * the engine keeps them beside the shelf. A catalog's credential lives
 * in [Credentials] under its origin, never here.
 *
 * The ids are the library's rows, carried as strings because that is
 * what a navigation route holds. Every call hops to the shelf's thread
 * and comes back through [scope] — never waited for, because this is
 * built on the main thread and the shelf's thread may be mid-import when
 * it is asked: the list is empty until the first read lands, and an add
 * answers through its callback once its row exists.
 */
class Catalogs(private val shelf: Shelf, private val scope: CoroutineScope) {
    private val _all = MutableStateFlow<List<SavedCatalog>>(emptyList())
    val all: StateFlow<List<SavedCatalog>> = _all

    init {
        scope.launch { refresh() }
    }

    fun get(id: String): SavedCatalog? = _all.value.firstOrNull { it.id == id }

    /**
     * Add a catalog; [title] may be blank until its feed says what it is
     * called. [onAdded] runs on the main thread with the row, after the
     * list shows it, so a screen can open it at once.
     */
    fun add(url: String, title: String = "", onAdded: (SavedCatalog) -> Unit) {
        scope.launch {
            val added = shelf.withApp { addCatalog(url, title) }?.saved() ?: return@launch
            refresh()
            onAdded(added)
        }
    }

    fun rename(id: String, title: String) {
        scope.launch {
            shelf.withApp { renameCatalog(id.toLong(), title) }
            refresh()
        }
    }

    fun remove(id: String) {
        scope.launch {
            shelf.withApp { removeCatalog(id.toLong()) }
            refresh()
        }
    }

    private suspend fun refresh() {
        _all.value = shelf.withApp { catalogs().map { it.saved() } }
    }

    private fun App.CatalogRecord.saved() = SavedCatalog(id.toString(), title, url)
}
