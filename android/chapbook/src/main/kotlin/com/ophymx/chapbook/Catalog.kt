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

/**
 * Everything needed to fetch one book yourself, for a download that has
 * to outlive the screen that started it.
 *
 * [Catalog.download] runs the whole transfer inside one blocking call,
 * which is fine for a tap the reader is watching and wrong for anything
 * else: the process has to stay alive for it. Take one of these instead,
 * put it in a `WorkManager` job, and call [Library.importFile] when the
 * file lands.
 *
 * **Every field is advice except [url].** Rename the file, add your own
 * headers, route through whatever the app routes through. The engine
 * reads a book by its bytes, so the name it arrives under does not
 * matter.
 *
 * **No credential travels in here, deliberately.** Your app opened this
 * catalog, so it already knows which credential the catalog takes — add
 * the `Authorization` header when the worker runs. That keeps the secret
 * out of `WorkManager`'s input `Data`, which is on disk, and means a
 * token rotated between enqueueing and running is simply fresh.
 *
 * [progressionUrl] and [annotationContainer] are the reason this type
 * exists at all. They live in the catalog entry and nowhere else, and by
 * the time a background download lands the feed is usually gone — so
 * they are captured here, persisted with the job, and handed to
 * [Library.setSyncTargets] after the import.
 */
data class DownloadRequest(
    /** The acquisition to fetch. The one field that is not advice. */
    val url: String,
    /** Send this with the request, and whatever else your app sends. */
    val headers: Map<String, String>,
    /** One safe path component, for a notification or a Downloads entry. */
    val suggestedFilename: String,
    /** What the catalog claims the file is. A hint for your UI only. */
    val mediaType: String?,
    /** The entry's title, so a notification can name the book. */
    val title: String,
    /** The entry's OPDS id: opaque, a key for your own job row. Never a path. */
    val entryId: String,
    /** The position-sync service, or null. */
    val progressionUrl: String?,
    /** The Web Annotation container, or null. */
    val annotationContainer: String?,
)

/**
 * One way to narrow the held feed, as the catalog offers it.
 *
 * Facets come in groups — "Language", "Sort by" — and the facets of a
 * group are alternatives, so a browse screen draws one control per
 * [group] with [active] marking the one in force. [href] is a feed:
 * hand it to [Catalog.fetch].
 */
data class Facet(
    val index: Int,
    val label: String,
    /** The group's name, and its position among the groups. */
    val group: String,
    val groupIndex: Int,
    val href: String,
    val active: Boolean,
    /** How many entries it would show, when the catalog says. */
    val count: Long?,
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

    /** The held feed's facets, in feed order, grouped as the catalog groups them. */
    fun facets(): List<Facet> {
        val flat = Native.catalogFacets(handle)
        return (0 until flat.size / 4).mapNotNull { index ->
            val base = index * 4
            Facet(
                index = index,
                label = Native.catalogFacetText(handle, index, 0) ?: return@mapNotNull null,
                group = Native.catalogFacetText(handle, index, 1) ?: "",
                groupIndex = flat[base].toInt(),
                href = Native.catalogFacetText(handle, index, 2) ?: return@mapNotNull null,
                active = flat[base + 1] != 0L,
                count = flat[base + 2].takeIf { flat[base + 3] != 0L },
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
     * Blocking and slow: it is a whole book over the network, inside
     * this call — so it is also the wrong call for a download that must
     * survive the app being backgrounded. For that use
     * [downloadRequest] with `WorkManager`, then [Library.importFile] and
     * [Library.setSyncTargets]. This is exactly those steps run back to
     * back, which is why the services have to be read before a transfer
     * that will outlive the feed.
     */
    fun download(entry: CatalogEntry, libraryDir: String): Long? =
        Native.catalogDownload(handle, entry.index, libraryDir).takeIf { it > 0 }

    /**
     * Describe an entry's download so the app can run it itself, or null
     * where the row has nothing to fetch.
     *
     * Cheap and local: it reads the held feed and touches no network. Do
     * it while the catalog is open, because the entry is the only place
     * the sync services exist — see [DownloadRequest].
     */
    fun downloadRequest(entry: CatalogEntry): DownloadRequest? =
        downloadRequest(entry.index)

    /** [downloadRequest] by index, for a host that kept only that. */
    fun downloadRequest(index: Int): DownloadRequest? {
        val url = Native.catalogEntryText(handle, index, 9) ?: return null
        return DownloadRequest(
            url = url,
            // One constant header, assembled here rather than crossed:
            // catalog servers negotiate by naive substring match, so this
            // is exactly what the engine would have sent.
            headers = mapOf("Accept" to "*/*"),
            suggestedFilename = Native.catalogEntryText(handle, index, 10) ?: "book",
            mediaType = Native.catalogEntryText(handle, index, 11),
            title = Native.catalogEntryText(handle, index, 0) ?: "",
            entryId = Native.catalogEntryText(handle, index, 8) ?: "",
            progressionUrl = Native.catalogEntryText(handle, index, 12),
            annotationContainer = Native.catalogEntryText(handle, index, 13),
        )
    }

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
