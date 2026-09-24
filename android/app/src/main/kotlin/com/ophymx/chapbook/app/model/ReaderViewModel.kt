package com.ophymx.chapbook.app.model

import android.app.Application
import android.content.ComponentCallbacks2
import android.content.res.Configuration
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.ophymx.chapbook.Book
import com.ophymx.chapbook.Position
import com.ophymx.chapbook.Session
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Where the reader is, for the chrome. Updated after every draw. */
data class Place(
    val title: String = "",
    val spine: Int = 0,
    val spineLen: Int = 0,
    val page: Int = 0,
    val pageCount: Int = 0,
)

sealed class ReaderState {
    data object Opening : ReaderState()
    data class Reading(val session: Session, val book: Book) : ReaderState()
    data object Gone : ReaderState()
}

/**
 * One open book, for as long as its screen exists.
 *
 * The session lives here rather than in the view because a rotation
 * recreates the view and must not reopen the book: the ViewModel
 * outlives the activity, the session moves to whichever view is
 * current, and it is closed exactly once, when the screen is popped.
 * The engine's rule is that a session is touched by one thread at a
 * time: it is opened on an IO thread and, once handed over, only ever
 * touched from the main thread.
 */
class ReaderViewModel(
    private val app: Application,
    private val bookId: Long,
    private val shelf: Shelf,
    private val opener: Opener,
) : ViewModel(), ComponentCallbacks2 {

    private val _state = MutableStateFlow<ReaderState>(ReaderState.Opening)
    val state: StateFlow<ReaderState> = _state.asStateFlow()

    private val _place = MutableStateFlow(Place())
    val place: StateFlow<Place> = _place.asStateFlow()

    private val session: Session?
        get() = (_state.value as? ReaderState.Reading)?.session

    init {
        app.registerComponentCallbacks(this)
        viewModelScope.launch {
            val book = shelf.book(bookId)
            val session = book?.let { withContext(Dispatchers.IO) { opener.open(it) } }
            if (book == null || session == null) {
                _state.value = ReaderState.Gone
                return@launch
            }
            // The engine's default budget is a desktop's. A quarter of
            // what the platform says this app may use is generous for a
            // page cache and leaves the rest for the screen.
            session.cacheBudget = memoryBudget()
            _state.value = ReaderState.Reading(session, book)
        }
    }

    private fun memoryBudget(): Long {
        val manager = app.getSystemService(android.app.ActivityManager::class.java)
        val classMb = manager?.memoryClass ?: 64
        return (classMb.toLong() * 1024 * 1024 / 4).coerceAtLeast(16L * 1024 * 1024)
    }

    /** Called by the page after each draw, which is when a position is authoritative. */
    fun moved(position: Position) {
        val s = session ?: return
        _place.value = Place(
            title = s.title,
            spine = position.spine,
            spineLen = s.spineLen,
            page = position.page,
            pageCount = s.pageCount,
        )
    }

    /** `onStop`: the last callback Android guarantees. */
    fun stopped() {
        session?.suspend()
    }

    override fun onTrimMemory(level: Int) {
        val s = session ?: return
        // Lowering the budget evicts at once; halving it on a real
        // warning keeps the next warning from finding the same cache.
        if (level >= ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW) {
            s.cacheBudget = (s.cacheBudget / 2).coerceAtLeast(4L * 1024 * 1024)
        }
        s.releaseCaches()
    }

    override fun onConfigurationChanged(newConfig: Configuration) = Unit

    @Deprecated("Android 14 no longer calls it; onTrimMemory covers it")
    override fun onLowMemory() = onTrimMemory(ComponentCallbacks2.TRIM_MEMORY_COMPLETE)

    override fun onCleared() {
        app.unregisterComponentCallbacks(this)
        session?.let {
            it.savePosition()
            it.close()
        }
        _state.value = ReaderState.Gone
    }

    companion object {
        fun factory(app: Application, container: AppContainer, bookId: Long): ViewModelProvider.Factory =
            viewModelFactory {
                initializer { ReaderViewModel(app, bookId, container.shelf, container.opener) }
            }
    }
}
