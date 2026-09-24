package com.ophymx.chapbook.app.model

import com.ophymx.chapbook.App
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/** A catalog the reader added: where it is and what it called itself. */
data class SavedCatalog(val id: String, val title: String, val url: String)

/**
 * The catalogs the reader has added, in the order they were added, as
 * the engine keeps them beside the shelf. A catalog's credential lives
 * in [Credentials] under its origin, never here.
 *
 * The ids are the library's rows, carried as strings because that is
 * what a navigation route holds. Each call is one small query on the
 * shelf's thread, waited for: an add has to answer with its row so the
 * screen can open it, and the wait is shorter than a frame.
 */
class Catalogs(private val shelf: Shelf) {
    private val _all = MutableStateFlow<List<SavedCatalog>>(emptyList())
    val all: StateFlow<List<SavedCatalog>> = _all

    init {
        refresh()
    }

    fun get(id: String): SavedCatalog? = _all.value.firstOrNull { it.id == id }

    /** Add a catalog; [title] may be blank until its feed says what it is called. */
    fun add(url: String, title: String = ""): SavedCatalog {
        val added = checkNotNull(shelf.blockingApp { addCatalog(url, title) }) { "the catalog could not be added; see logcat" }
        refresh()
        return added.saved()
    }

    fun rename(id: String, title: String) {
        shelf.blockingApp { renameCatalog(id.toLong(), title) }
        refresh()
    }

    fun remove(id: String) {
        shelf.blockingApp { removeCatalog(id.toLong()) }
        refresh()
    }

    private fun refresh() {
        _all.value = shelf.blockingApp { catalogs().map { it.saved() } }
    }

    private fun App.CatalogRecord.saved() = SavedCatalog(id.toString(), title, url)
}
