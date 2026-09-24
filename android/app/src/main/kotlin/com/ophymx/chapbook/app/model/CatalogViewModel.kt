package com.ophymx.chapbook.app.model

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.ophymx.chapbook.CatalogEntry
import com.ophymx.chapbook.CatalogError
import com.ophymx.chapbook.Facet
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** One feed on screen, and the crumb trail that led to it. */
data class Browsing(
    val url: String,
    val title: String = "",
    val entries: List<CatalogEntry> = emptyList(),
    val facets: List<Facet> = emptyList(),
    val hasSearch: Boolean = false,
    val nextPage: String? = null,
    val loading: Boolean = true,
    /** Rows appended by paging, held apart so a facet change can replace cleanly. */
    val loadingMore: Boolean = false,
)

sealed class CatalogUi {
    data object Opening : CatalogUi()
    data class Feed(val feed: Browsing) : CatalogUi()

    /** A 401 with a login to draw. */
    data class Login(val title: String, val offersBasic: Boolean, val retryUrl: String) : CatalogUi()
    data class Failed(val url: String) : CatalogUi()
}

/**
 * A browsing session over one saved catalog.
 *
 * The [CatalogSession] owns the blocking catalog on its own thread; this
 * turns feeds into screens and taps into fetches. A navigation row is a
 * fetch that pushes a crumb; a publication row is a download the app
 * enqueues; a facet or a page is a fetch that replaces or appends. A 401
 * surfaces as a login rather than a failure, and a sign-in stores the
 * credential by origin and fetches again.
 */
class CatalogViewModel(
    private val saved: SavedCatalog,
    private val session: CatalogSession,
    private val credentials: Credentials,
    private val downloads: Downloads,
) : ViewModel() {

    private val _ui = MutableStateFlow<CatalogUi>(CatalogUi.Opening)
    val ui: StateFlow<CatalogUi> = _ui.asStateFlow()

    private val crumbs = ArrayDeque<String>()

    init {
        open(saved.url)
    }

    private fun open(url: String, pushCrumb: Boolean = true) {
        viewModelScope.launch {
            if (pushCrumb) crumbs.addLast(url)
            _ui.value = CatalogUi.Feed(Browsing(url = url, loading = true))
            _ui.value = fetch(url)
        }
    }

    private suspend fun fetch(url: String): CatalogUi = try {
        session.use {
            fetch(url)
            CatalogUi.Feed(
                Browsing(
                    url = url,
                    title = feedTitle ?: saved.title,
                    entries = entries(),
                    facets = facets(),
                    hasSearch = hasSearch,
                    nextPage = nextPage,
                    loading = false,
                ),
            )
        }
    } catch (e: CatalogError.AuthRequired) {
        session.use {
            CatalogUi.Login(authTitle ?: saved.title, authOffersBasic, url)
        }
    } catch (e: CatalogError) {
        CatalogUi.Failed(url)
    }

    fun openEntry(entry: CatalogEntry) {
        if (entry.kind == com.ophymx.chapbook.EntryKind.NAVIGATION) {
            entry.href?.let { open(it) }
        }
    }

    /** Enqueue a publication's download, described while the feed is open. */
    fun download(entry: CatalogEntry, onQueued: () -> Unit) {
        viewModelScope.launch {
            val request = session.use { downloadRequest(entry) } ?: return@launch
            downloads.enqueue(request)
            onQueued()
        }
    }

    fun applyFacet(facet: Facet) = open(facet.href)

    fun search(query: String) {
        viewModelScope.launch {
            _ui.update { (it as? CatalogUi.Feed)?.copy(feed = it.feed.copy(loading = true)) ?: it }
            _ui.value = try {
                session.use {
                    search(query)
                    CatalogUi.Feed(
                        Browsing(
                            url = (_ui.value as? CatalogUi.Feed)?.feed?.url ?: saved.url,
                            title = feedTitle ?: saved.title,
                            entries = entries(),
                            facets = facets(),
                            hasSearch = hasSearch,
                            nextPage = nextPage,
                            loading = false,
                        ),
                    )
                }
            } catch (e: CatalogError) {
                CatalogUi.Failed(saved.url)
            }
        }
    }

    fun loadMore() {
        val feed = (_ui.value as? CatalogUi.Feed)?.feed ?: return
        val next = feed.nextPage ?: return
        if (feed.loadingMore) return
        viewModelScope.launch {
            _ui.update { (it as? CatalogUi.Feed)?.copy(feed = it.feed.copy(loadingMore = true)) ?: it }
            val more = try {
                session.use {
                    fetch(next)
                    Triple(entries(), nextPage, facets())
                }
            } catch (e: CatalogError) {
                null
            }
            _ui.update { state ->
                val f = (state as? CatalogUi.Feed)?.feed ?: return@update state
                if (more == null) {
                    CatalogUi.Feed(f.copy(loadingMore = false))
                } else {
                    CatalogUi.Feed(f.copy(entries = f.entries + more.first, nextPage = more.second, loadingMore = false))
                }
            }
        }
    }

    /** Sign in with Basic, store it by origin, and fetch the refused feed again. */
    fun signIn(username: String, password: String, retryUrl: String) {
        Credentials.originOf(retryUrl)?.let { origin ->
            credentials.set(origin, Credentials.basic(username, password))
        }
        open(retryUrl, pushCrumb = false)
    }

    /** True when Back stayed inside the catalog; false when the screen should close. */
    fun back(): Boolean {
        if (crumbs.size <= 1) return false
        crumbs.removeLast()
        open(crumbs.last(), pushCrumb = false)
        return true
    }

    override fun onCleared() {
        session.close()
    }

    companion object {
        fun factory(container: AppContainer, saved: SavedCatalog): ViewModelProvider.Factory = viewModelFactory {
            initializer {
                CatalogViewModel(saved, CatalogSession(container.http.transport), container.credentials, container.downloads)
            }
        }
    }
}
