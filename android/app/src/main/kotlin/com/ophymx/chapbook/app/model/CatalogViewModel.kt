package com.ophymx.chapbook.app.model

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.ophymx.chapbook.Catalog
import com.ophymx.chapbook.CatalogEntry
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
 * A browsing session over one saved catalog, as a screen sees it.
 *
 * The decisions are the engine's: a navigation row pushes a crumb, Back
 * walks the crumbs before it leaves the screen, a facet replaces and a
 * page appends, a 401 is a login rather than a failure, and a sign-in
 * stores the credential by origin and fetches again. This turns each
 * of those into a hop onto the catalog's thread and a snapshot of what
 * it holds afterwards. The one thing mirrored here is the crumb depth,
 * so Back can answer at once whether it stays inside the catalog.
 */
class CatalogViewModel(
    private val saved: SavedCatalog,
    private val session: CatalogSession,
    private val downloads: Downloads,
) : ViewModel() {

    private val _ui = MutableStateFlow<CatalogUi>(CatalogUi.Opening)
    val ui: StateFlow<CatalogUi> = _ui.asStateFlow()

    /** How many crumbs the engine's trail holds — every [Catalog.go] pushes one. */
    private var depth = 0

    init {
        go(saved.url)
    }

    /** What the catalog holds, as the screen draws it. */
    private fun Catalog.snapshot(url: String): CatalogUi = when (val s = state) {
        Catalog.BrowseState.Feed -> CatalogUi.Feed(
            Browsing(
                url = this.url.ifEmpty { url },
                title = title.ifBlank { saved.title },
                entries = entries(),
                facets = facets(),
                hasSearch = hasSearch,
                nextPage = nextPage,
                loading = false,
            ),
        )
        is Catalog.BrowseState.Login -> CatalogUi.Login(s.title.ifBlank { saved.title }, s.offersBasic, s.retryUrl)
        is Catalog.BrowseState.Failed -> CatalogUi.Failed(s.url)
        Catalog.BrowseState.Opening -> CatalogUi.Failed(url)
    }

    /** Open a feed, pushing a crumb. */
    private fun go(url: String) {
        depth++
        viewModelScope.launch {
            _ui.value = CatalogUi.Feed(Browsing(url = url, loading = true))
            _ui.value = try {
                session.use {
                    runCatching { go(url) }
                    snapshot(url)
                }
            } catch (e: Exception) {
                CatalogUi.Failed(url)
            }
        }
    }

    fun openEntry(entry: CatalogEntry) {
        if (entry.kind == com.ophymx.chapbook.EntryKind.NAVIGATION) {
            entry.href?.let { go(it) }
        }
    }

    /**
     * Enqueue a publication's download. The entry carries its own request,
     * captured when its row was read, so this needs neither the catalog
     * nor the feed it came from — which may be pages back.
     */
    fun download(entry: CatalogEntry, onQueued: () -> Unit) {
        val request = entry.download ?: return
        downloads.enqueue(request)
        onQueued()
    }

    fun applyFacet(facet: Facet) {
        depth++
        viewModelScope.launch {
            _ui.update { (it as? CatalogUi.Feed)?.copy(feed = it.feed.copy(loading = true)) ?: it }
            _ui.value = try {
                session.use {
                    runCatching { applyFacet(facet) }
                    snapshot(facet.href)
                }
            } catch (e: Exception) {
                CatalogUi.Failed(facet.href)
            }
        }
    }

    fun search(query: String) {
        viewModelScope.launch {
            val url = (_ui.value as? CatalogUi.Feed)?.feed?.url ?: saved.url
            _ui.update { (it as? CatalogUi.Feed)?.copy(feed = it.feed.copy(loading = true)) ?: it }
            _ui.value = try {
                session.use {
                    runCatching { search(query) }
                    snapshot(url)
                }
            } catch (e: Exception) {
                CatalogUi.Failed(saved.url)
            }
        }
    }

    fun loadMore() {
        val feed = (_ui.value as? CatalogUi.Feed)?.feed ?: return
        if (feed.nextPage == null || feed.loadingMore) return
        viewModelScope.launch {
            _ui.update { (it as? CatalogUi.Feed)?.copy(feed = it.feed.copy(loadingMore = true)) ?: it }
            val more = try {
                session.use {
                    if (runCatching { loadMore() }.getOrDefault(false)) entries() to nextPage else null
                }
            } catch (e: Exception) {
                null
            }
            _ui.update { state ->
                val f = (state as? CatalogUi.Feed)?.feed ?: return@update state
                if (more == null) {
                    CatalogUi.Feed(f.copy(loadingMore = false))
                } else {
                    CatalogUi.Feed(f.copy(entries = more.first, nextPage = more.second, loadingMore = false))
                }
            }
        }
    }

    /** Sign in with Basic: the engine stores it by origin and fetches the refused feed again. */
    fun signIn(username: String, password: String, retryUrl: String) {
        viewModelScope.launch {
            _ui.value = CatalogUi.Feed(Browsing(url = retryUrl, loading = true))
            _ui.value = try {
                session.use {
                    runCatching { submitLogin(username, password) }
                    snapshot(retryUrl)
                }
            } catch (e: Exception) {
                CatalogUi.Failed(retryUrl)
            }
        }
    }

    /** True when Back stayed inside the catalog; false when the screen should close. */
    fun back(): Boolean {
        if (depth <= 1) return false
        depth--
        viewModelScope.launch {
            _ui.update { (it as? CatalogUi.Feed)?.copy(feed = it.feed.copy(loading = true)) ?: it }
            _ui.value = try {
                session.use {
                    back()
                    snapshot(url)
                }
            } catch (e: Exception) {
                CatalogUi.Failed(saved.url)
            }
        }
        return true
    }

    override fun onCleared() {
        session.close()
    }

    companion object {
        fun factory(container: AppContainer, saved: SavedCatalog): ViewModelProvider.Factory = viewModelFactory {
            initializer {
                CatalogViewModel(saved, CatalogSession(container.shelf, saved.id.toLongOrNull() ?: 0L), container.downloads)
            }
        }
    }
}
