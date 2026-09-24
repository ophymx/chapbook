package com.ophymx.chapbook.app.model

import android.app.Application
import android.content.ComponentCallbacks2
import android.content.res.Configuration
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.ophymx.chapbook.Annotation
import com.ophymx.chapbook.Book
import com.ophymx.chapbook.BookKind
import com.ophymx.chapbook.Locator
import com.ophymx.chapbook.Position
import com.ophymx.chapbook.ReadingSettings
import com.ophymx.chapbook.SearchHit
import com.ophymx.chapbook.Session
import com.ophymx.chapbook.TocEntry
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.yield

/** Where the reader is, for the chrome. Updated after every draw. */
data class Place(
    val title: String = "",
    val spine: Int = 0,
    val spineLen: Int = 0,
    val page: Int = 0,
    val pageCount: Int = 0,
    /**
     * Whole-book progress, 0..1, spine-weighted the way the engine's own
     * `book_progression` is: each unit a `1/spineLen` slice, the page's
     * place within it added. The shell has every term, so the bar needs
     * no new binding call.
     */
    val bookFraction: Float = 0f,
    /** Whether the engine's Back has anywhere to go — after a link. */
    val canGoBack: Boolean = false,
)

sealed class ReaderState {
    data object Opening : ReaderState()

    data class Reading(
        val session: Session,
        val book: Book,
        val kind: BookKind,
        /** Read once at open; a book's contents do not change. */
        val toc: List<TocEntry>,
        /** Every family the session's fonts offer, for the picker. */
        val fontFamilies: List<String>,
    ) : ReaderState()

    data object Gone : ReaderState()
}

/** A search in progress or finished. */
data class SearchState(
    val query: String = "",
    val hits: List<SearchHit> = emptyList(),
    val running: Boolean = false,
)

/** The reader's text selection, as the chrome sees it. */
data class Selection(val start: Int, val end: Int, val text: String)

/**
 * One open book, for as long as its screen exists.
 *
 * The session lives here rather than in the view because a rotation
 * recreates the view and must not reopen the book: the ViewModel
 * outlives the activity, the session moves to whichever view is
 * current, and it is closed exactly once, when the screen is popped.
 * The engine's rule is that a session is touched by one thread at a
 * time: it is opened on an IO thread and, once handed over, only ever
 * touched from the main thread — which is why a search here walks the
 * book unit by unit on the main thread, yielding between units, rather
 * than blocking a worker that would race the next draw.
 *
 * Everything that changes what the page shows ends by asking the view
 * to redraw through [onNeedsRedraw]; the view installs that while it is
 * on screen.
 */
class ReaderViewModel(
    private val app: Application,
    private val bookId: Long,
    private val shelf: Shelf,
    private val opener: Opener,
    private val preferences: Preferences,
) : ViewModel(), ComponentCallbacks2 {

    val progressLabel: StateFlow<ProgressLabel> = preferences.progressLabel

    fun setProgressLabel(label: ProgressLabel) = preferences.setProgressLabel(label)

    private val _state = MutableStateFlow<ReaderState>(ReaderState.Opening)
    val state: StateFlow<ReaderState> = _state.asStateFlow()

    private val _place = MutableStateFlow(Place())
    val place: StateFlow<Place> = _place.asStateFlow()

    private val _settings = MutableStateFlow<ReadingSettings?>(null)
    val settings: StateFlow<ReadingSettings?> = _settings.asStateFlow()

    private val _fontFamily = MutableStateFlow<String?>(null)
    val fontFamily: StateFlow<String?> = _fontFamily.asStateFlow()

    private val _marks = MutableStateFlow<List<Annotation>>(emptyList())
    val marks: StateFlow<List<Annotation>> = _marks.asStateFlow()

    private val _search = MutableStateFlow(SearchState())
    val search: StateFlow<SearchState> = _search.asStateFlow()

    private val _selection = MutableStateFlow<Selection?>(null)
    val selection: StateFlow<Selection?> = _selection.asStateFlow()

    /** Installed by the page while it is showing. */
    var onNeedsRedraw: (() -> Unit)? = null

    private var searching: Job? = null

    private val session: Session?
        get() = (_state.value as? ReaderState.Reading)?.session

    init {
        app.registerComponentCallbacks(this)
        viewModelScope.launch {
            val book = shelf.book(bookId)
            val opened = book?.let {
                withContext(Dispatchers.IO) {
                    opener.open(it)?.let { session ->
                        // Read on the same thread that opened, before the
                        // hand-over: the contents and the font list do
                        // not change and both cost a little.
                        Triple(session, session.toc(), session.fontFamilies.toList())
                    }
                }
            }
            if (book == null || opened == null) {
                _state.value = ReaderState.Gone
                return@launch
            }
            val (session, toc, families) = opened
            // The engine's default budget is a desktop's. A quarter of
            // what the platform says this app may use is generous for a
            // page cache and leaves the rest for the screen.
            session.cacheBudget = memoryBudget()
            _settings.value = session.settings
            _fontFamily.value = session.fontFamily
            _marks.value = session.annotations()
            _state.value = ReaderState.Reading(session, book, session.kind, toc, families)
        }
    }

    private fun memoryBudget(): Long {
        val manager = app.getSystemService(android.app.ActivityManager::class.java)
        val classMb = manager?.memoryClass ?: 64
        return (classMb.toLong() * 1024 * 1024 / 4).coerceAtLeast(16L * 1024 * 1024)
    }

    private fun redraw() = onNeedsRedraw?.invoke()

    // ---- Where the reader is ----

    /** Called by the page after each draw, which is when a position is authoritative. */
    fun moved(position: Position) {
        val s = session ?: return
        val spineLen = s.spineLen
        val pageCount = s.pageCount
        val within = if (pageCount > 0) position.page.toFloat() / pageCount else 0f
        val bookFraction = if (spineLen > 0) ((position.spine + within) / spineLen).coerceIn(0f, 1f) else 0f
        _place.value = Place(
            title = s.title,
            spine = position.spine,
            spineLen = spineLen,
            page = position.page,
            pageCount = pageCount,
            bookFraction = bookFraction,
            canGoBack = s.canGoBack,
        )
        // Settings can change under a page turn — `font-up` from a
        // pinch — so the sheet's numbers follow the draw too.
        _settings.value = s.settings
    }

    /** The engine's Back: where the reader was before the last link. */
    fun goBack() {
        val s = session ?: return
        if (s.apply("back").needsRedraw) redraw()
    }

    fun gotoToc(entry: TocEntry) {
        val s = session ?: return
        if (s.gotoToc(entry)) redraw()
    }

    // ---- Settings ----

    fun applySettings(settings: ReadingSettings, thisBook: Boolean) {
        val s = session ?: return
        s.setSettings(settings, thisBook)
        _settings.value = s.settings
        redraw()
    }

    fun setFontFamily(family: String?, thisBook: Boolean) {
        val s = session ?: return
        s.setFontFamily(family, thisBook)
        _fontFamily.value = s.fontFamily
        redraw()
    }

    /** Drop this book's own settings so it follows the defaults again. */
    fun resetBookSettings() {
        val s = session ?: return
        s.clearBookSettings()
        _settings.value = s.settings
        _fontFamily.value = s.fontFamily
        redraw()
    }

    // ---- Marks ----

    private fun refreshMarks() {
        _marks.value = session?.annotations() ?: emptyList()
    }

    fun addBookmark() {
        session?.addBookmark()
        refreshMarks()
    }

    /** The selection becomes a highlight, and the selection goes. */
    fun highlightSelection() {
        val s = session ?: return
        s.addHighlight()
        s.selectionClear()
        _selection.value = null
        refreshMarks()
        redraw()
    }

    fun noteOnSelection(body: String) {
        val s = session ?: return
        s.addNote(body)
        s.selectionClear()
        _selection.value = null
        refreshMarks()
        redraw()
    }

    fun removeMark(id: Long) {
        session?.removeAnnotation(id)
        refreshMarks()
        redraw()
    }

    fun gotoMark(id: Long) {
        val s = session ?: return
        if (s.gotoAnnotation(id)) redraw()
    }

    fun recolorHighlight(id: Long, color: String?) {
        session?.setHighlightColor(id, color)
        refreshMarks()
        redraw()
    }

    // ---- Selection ----

    /** The page reports what is selected after each draw; null clears. */
    fun selected(start: Int, end: Int) {
        val s = session ?: return
        _selection.value = Selection(start, end, s.selectedText ?: "")
    }

    fun clearSelection() {
        val s = session ?: return
        if (s.selectedRange != null) {
            s.selectionClear()
            redraw()
        }
        _selection.value = null
    }

    // ---- Search ----

    /**
     * Search the book, one unit at a time on the main thread, yielding
     * between units so the page stays responsive. The blocking
     * whole-book `search` would want a worker, and a worker would touch
     * the session while the page draws — the one rule the engine has.
     */
    fun search(query: String) {
        searching?.cancel()
        val trimmed = query.trim()
        if (trimmed.isEmpty()) {
            _search.value = SearchState()
            return
        }
        _search.value = SearchState(query = trimmed, running = true)
        searching = viewModelScope.launch {
            val s = session ?: return@launch
            val hits = ArrayList<SearchHit>()
            for (spine in 0 until s.spineLen) {
                hits += s.searchUnit(spine, trimmed)
                _search.update { it.copy(hits = hits.toList()) }
                if (hits.size >= SEARCH_CAP) break
                yield()
            }
            _search.update { it.copy(running = false) }
        }
    }

    fun clearSearch() {
        searching?.cancel()
        _search.value = SearchState()
    }

    /** Jump to a hit and leave it selected, so the eye finds it. */
    fun gotoHit(hit: SearchHit) {
        val s = session ?: return
        s.goto(Locator(hit.spine, hit.start))
        s.selectRange(hit.start, hit.end)
        redraw()
    }

    // ---- Lifecycle ----

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
        searching?.cancel()
        session?.let {
            it.savePosition()
            it.close()
        }
        _state.value = ReaderState.Gone
    }

    companion object {
        private const val SEARCH_CAP = 200

        fun factory(app: Application, container: AppContainer, bookId: Long): ViewModelProvider.Factory =
            viewModelFactory {
                initializer { ReaderViewModel(app, bookId, container.shelf, container.opener, container.preferences) }
            }
    }
}
