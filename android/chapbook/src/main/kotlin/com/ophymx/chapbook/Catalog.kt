package com.ophymx.chapbook

/** What a catalog row is, which decides what tapping it does. */
enum class EntryKind { NAVIGATION, PUBLICATION }

/** One row of a catalog listing. */
data class CatalogEntry(
    val index: Int,
    val kind: EntryKind,
    val title: String,
    val authors: List<String>,
    val summary: String?,
    val series: String?,
    /** Absolute URL — load it with Coil, Glide, whatever the app uses. */
    val thumbnailUrl: String?,
    /** Where tapping goes: a feed to fetch, or the acquisition. */
    val href: String?,
    /** Whether [Catalog.download] can take this one. */
    val canDownload: Boolean,
    /** Free, as against borrowed or bought — a "Get" button, not "Buy". */
    val isOpenAccess: Boolean,
    /** Whether the entry advertises the two sync services. */
    val syncsPosition: Boolean,
    val syncsAnnotations: Boolean,
)

/** Why a catalog call did not succeed. */
sealed class CatalogError : Exception() {
    /** The network failed, or the catalog answered something unusable. */
    data object Unreachable : CatalogError()

    /**
     * The catalog wants credentials and said so properly. Read
     * [Catalog.authTitle] and [Catalog.authOffersBasic], put up a login,
     * call [Catalog.signIn], and fetch again.
     */
    data object AuthRequired : CatalogError()

    /** This catalog offers no search. */
    data object NoSearch : CatalogError()
}

/**
 * An OPDS catalog, browsed one feed at a time.
 *
 * **Every call blocks.** Run them on `Dispatchers.IO`, where the app's
 * own cancellation already lives — this binding deliberately does not
 * invent a worker, because a coroutine is better than anything it could.
 *
 * The transport is the app's own ([SyncTransport]), so a catalog behind
 * a proxy, a user CA, or a corporate network works because the
 * platform's HTTP client does.
 */
class Catalog(transport: SyncTransport) : AutoCloseable {
    private var handle: Long = Native.catalogOpen(transport)

    init {
        check(handle != 0L) { "catalog did not open; see logcat" }
    }

    /** Send this `Authorization` with every request; null clears it. */
    fun setAuthorization(value: String?) = Native.catalogSetAuthorization(handle, value)

    /** Sign in with a username and password — the Basic flow. */
    fun signIn(username: String, password: String) =
        Native.catalogSetBasicAuth(handle, username, password)

    /** Fetch a feed and hold it. Blocking. */
    @Throws(CatalogError::class)
    fun fetch(url: String) = check(Native.catalogFetch(handle, url))

    /** Search the held catalog, replacing it with the results. */
    @Throws(CatalogError::class)
    fun search(query: String) = check(Native.catalogSearch(handle, query))

    private fun check(code: Int) {
        when (code) {
            0 -> Unit
            -2 -> throw CatalogError.AuthRequired
            -3 -> throw CatalogError.NoSearch
            else -> throw CatalogError.Unreachable
        }
    }

    /** The held feed's title — what a browse screen puts at the top. */
    val feedTitle: String? get() = Native.catalogFeedTitle(handle)

    /** Whether this catalog offers a search, so a box is worth drawing. */
    val hasSearch: Boolean get() = Native.catalogHasSearch(handle)

    /** Every row of the held feed. */
    fun entries(): List<CatalogEntry> {
        val count = Native.catalogEntryCount(handle)
        if (count <= 0) return emptyList()
        return (0 until count).mapNotNull { index ->
            val flags = Native.catalogEntry(handle, index)
            if (flags.size < 8) return@mapNotNull null
            CatalogEntry(
                index = index,
                kind = if (flags[0] != 0L) EntryKind.PUBLICATION else EntryKind.NAVIGATION,
                title = Native.catalogEntryText(handle, index, 0) ?: "",
                authors = (0 until flags[1].toInt()).mapNotNull {
                    Native.catalogEntryAuthor(handle, index, it)
                },
                summary = Native.catalogEntryText(handle, index, 1),
                series = Native.catalogEntryText(handle, index, 4),
                thumbnailUrl = Native.catalogEntryText(handle, index, 5),
                href = Native.catalogEntryText(handle, index, 7),
                canDownload = flags[2] != 0L,
                isOpenAccess = flags[3] != 0L,
                syncsPosition = flags[6] != 0L,
                syncsAnnotations = flags[7] != 0L,
            )
        }
    }

    /** The next page's URL, or null at the end of a paged feed. */
    val nextPage: String? get() = Native.catalogPageHref(handle, 0)

    /** The previous page's URL, or null. */
    val previousPage: String? get() = Native.catalogPageHref(handle, 1)

    /**
     * Put an entry on the shelf: fetch it, import it, **and record the
     * sync services it advertises** — which live in the catalog entry
     * and nowhere else, so a book added any other way is one that will
     * never reconcile. Returns the library row, or null.
     *
     * Blocking and slow: it is a whole book over the network.
     */
    fun download(entry: CatalogEntry, libraryDir: String): Long? =
        Native.catalogDownload(handle, entry.index, libraryDir).takeIf { it > 0 }

    /** The refused catalog's own name, for a login sheet. */
    val authTitle: String? get() = Native.catalogAuthTitle(handle)

    /**
     * Whether the refused catalog offers username-and-password — the
     * only flow a reader can complete without a browser. False means it
     * wants something else, and a host should say so rather than show a
     * login that cannot work.
     */
    val authOffersBasic: Boolean get() = Native.catalogAuthOffersBasic(handle)

    override fun close() {
        if (handle != 0L) {
            Native.catalogClose(handle)
            handle = 0
        }
    }
}
