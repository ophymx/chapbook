package com.ophymx.chapbook.app.model

import android.net.Uri
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.ophymx.chapbook.Book
import com.ophymx.chapbook.BookQuery
import com.ophymx.chapbook.ReadingState
import com.ophymx.chapbook.Sort
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** What the shelf screen draws. */
data class ShelfState(
    val books: List<Book> = emptyList(),
    val search: String = "",
    val sort: Sort = Sort.Read,
    val state: ReadingState? = null,
    val loading: Boolean = true,
    /** Something to tell the reader once, or null. */
    val notice: Notice? = null,
)

/** A one-line message for the reader; the screen decides the words. */
enum class Notice { OpenFailed }

/**
 * The shelf's decisions: which books, in what order, and what adding
 * one does. The default sort is *recently read*, which is what the
 * desktop app and the CLI answer too — a shelf opens on the book the
 * reader was in.
 */
class ShelfViewModel(private val shelf: Shelf, private val opener: Opener) : ViewModel() {
    private val _state = MutableStateFlow(ShelfState())
    val state: StateFlow<ShelfState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            val s = _state.value
            val books = shelf.books(
                BookQuery(search = s.search.trim().ifEmpty { null }, state = s.state, sort = s.sort),
            )
            _state.update { it.copy(books = books, loading = false) }
        }
    }

    fun setSearch(search: String) {
        _state.update { it.copy(search = search) }
        refresh()
    }

    fun setSort(sort: Sort) {
        _state.update { it.copy(sort = sort) }
        refresh()
    }

    fun setStateFilter(state: ReadingState?) {
        _state.update { it.copy(state = state) }
        refresh()
    }

    /** Add a file and, if it is a book, say which row so the caller can open it. */
    fun add(uri: Uri, onAdded: (Long) -> Unit) {
        viewModelScope.launch {
            when (val added = opener.add(uri)) {
                is Added.Book -> {
                    refresh()
                    onAdded(added.id)
                }
                is Added.Failed -> _state.update { it.copy(notice = Notice.OpenFailed) }
            }
        }
    }

    fun setFinished(book: Book, finished: Boolean) {
        viewModelScope.launch {
            shelf.setFinished(book.id, finished)
            refresh()
        }
    }

    fun remove(book: Book) {
        viewModelScope.launch {
            shelf.remove(book.id)
            refresh()
        }
    }

    fun dismissNotice() = _state.update { it.copy(notice = null) }

    companion object {
        fun factory(container: AppContainer): ViewModelProvider.Factory = viewModelFactory {
            initializer { ShelfViewModel(container.shelf, container.opener) }
        }
    }
}
