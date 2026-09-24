package com.ophymx.chapbook

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList

/**
 * The background-download flow, taken apart the way a `WorkManager` job
 * has to run it: describe the fetch while the feed is open, run the
 * transfer elsewhere, hand the finished file back to the shelf, then
 * record the sync services the entry carried.
 *
 * These are the same properties the Swift binding pins in `swift test`,
 * because they are the properties the flow leans on rather than a
 * restatement of the API — and because the one bug this class of test
 * exists to catch already happened here: `catalogEntryText` used to
 * answer every unknown field with the href, and the download URL is the
 * field where the href is precisely the wrong answer.
 *
 * What nothing here reaches: a worker actually surviving a suspended
 * process. That is device-only, and it is the platform's promise.
 */
@RunWith(AndroidJUnit4::class)
class DownloadFlowTest {

    @Before
    fun logging() = Session.initLogging()

    // ---- The catalog, served from fixtures ----

    /** Fixture bytes, staged into the test APK by `build.gradle.kts`. */
    private fun fixture(path: String): ByteArray =
        InstrumentationRegistry.getInstrumentation().context.assets.open(path).use { it.readBytes() }

    /**
     * A transport that answers from the repository's fixtures and records
     * what was asked of it. One per test, so an absence checked here is
     * an absence in this test alone.
     */
    private inner class FixtureCatalog : SyncTransport {
        val seen = CopyOnWriteArrayList<String>()

        override fun get(url: String, headers: Array<String>): SyncResponse {
            seen += url
            val (status, type, body) = when (url) {
                ROOT -> Triple(200, NAVIGATION, fixture("opds/navigation.atom.xml"))
                SYNCING_SHELF -> Triple(200, ACQUISITION, fixture("opds/acquisition-sync.atom.xml"))
                else -> Triple(404, "text/plain", "no".toByteArray())
            }
            return SyncResponse(status, type, emptyArray(), body)
        }

        override fun send(method: String, url: String, headers: Array<String>, body: ByteArray): SyncResponse =
            throw UnsupportedOperationException("browsing never writes: $method $url")
    }

    // ---- The shelf, in a scratch directory ----

    private fun scratch(name: String): File =
        File(InstrumentationRegistry.getInstrumentation().targetContext.cacheDir, "$name-${System.nanoTime()}")
            .also { check(it.mkdirs()) { "could not create $it" } }

    /** A fixture book landed under whatever name the platform chose. */
    private fun handedOver(fixture: String, name: String, into: File): File =
        File(into, name).apply { writeBytes(fixture(fixture)) }

    /** JUnit's `assertNotNull` returns nothing; this is Swift's `#require`. */
    private fun <T : Any> required(value: T?, message: String = "expected a value"): T =
        value ?: throw AssertionError(message)

    private fun <T> withLibrary(name: String, body: (File, Library) -> T): T {
        val dir = scratch(name)
        try {
            val library = checkNotNull(Library.open(dir.absolutePath)) { "library did not open; see logcat" }
            return library.use { body(dir, it) }
        } finally {
            dir.deleteRecursively()
        }
    }

    // ---- Describing ----

    @Test
    fun aDownloadTheAppRunsItselfIsDescribedWhileTheFeedIsStillOpen() {
        val transport = FixtureCatalog()
        Catalog(transport).use { catalog ->
            catalog.fetch(SYNCING_SHELF)
            val entry = catalog.entries().first { it.kind == EntryKind.PUBLICATION }
            val request = required(catalog.downloadRequest(entry))

            // The one field that is not advice, and it crossed absolute — a
            // host resolves nothing itself.
            assertEquals("$ORIGIN/dl/sync/v3.epub", request.url)
            assertEquals("application/epub+zip", request.mediaType)
            assertEquals("Vol. 3: Rain / Thunder", request.title)

            // Opaque, and opaque means untouched: this id carries slashes,
            // and a host keys its own job record on it.
            assertEquals("urn:example:sync/demo/v3", request.entryId)

            // The reason this type exists. Both services live in the entry
            // and nowhere else, so they are captured here, while the feed
            // is open, rather than looked up when the transfer lands.
            //
            // The progression href is root-relative in the fixture and
            // absolute here: unresolved, it would reach `setSyncTargets`
            // as a path. The annotation container was already absolute.
            assertEquals("$ORIGIN/sync/position/v3", request.progressionUrl)
            assertEquals("$ORIGIN/sync/annotations/v3", request.annotationContainer)

            // One safe path component. The app writes this to a filesystem,
            // so a separator surviving the title would be a path traversal
            // wearing a filename — and the title in this fixture has one.
            assertTrue(request.suggestedFilename.isNotEmpty())
            assertFalse(request.suggestedFilename.contains('/'))
            assertFalse(request.suggestedFilename.contains(':'))
            assertEquals(File("/tmp"), File("/tmp", request.suggestedFilename).parentFile)

            // The engine's header, and only it. No `Authorization`: the
            // app opened this catalog, so it adds its own credential when
            // the worker runs, which keeps the secret out of `WorkManager`'s
            // on-disk input `Data`.
            assertEquals(mapOf("Accept" to "*/*"), request.headers)
            assertFalse(request.headers.keys.any { it.equals("Authorization", ignoreCase = true) })

            // Local and free: describing a download must not fetch it, or
            // an app building a list of jobs would download the shelf.
            assertEquals(listOf(SYNCING_SHELF), transport.seen)

            // By index, for a host that kept only that.
            assertEquals(request, catalog.downloadRequest(entry.index))

            // A navigation row has nothing to fetch and answers null
            // rather than throwing — a feed URL handed back here would
            // download as a book, which is why this reads the acquisition
            // and not `href`.
            catalog.fetch(ROOT)
            val section = catalog.entries().first { it.kind == EntryKind.NAVIGATION }
            assertNotNull("the row does have a link, just not one to fetch", section.href)
            assertNull(catalog.downloadRequest(section))
        }
    }

    /**
     * The bug this exists for: a screen that pages appends rows from feed
     * after feed, and a Get on a row from the first page used to describe
     * whatever the *current* feed held at that index. The entry carries
     * its own download now, so the row outlives the feed it came from.
     */
    @Test
    fun aDescribedDownloadOutlivesTheFeedItCameFrom() {
        Catalog(FixtureCatalog()).use { catalog ->
            catalog.fetch(SYNCING_SHELF)
            val entry = catalog.entries().first { it.kind == EntryKind.PUBLICATION }
            val request = required(catalog.downloadRequest(entry))
            assertEquals("$ORIGIN/dl/sync/v3.epub", request.url)

            // The next page arrives and the held feed is a different one:
            // the root, whose rows are all navigation.
            catalog.fetch(ROOT)
            assertEquals(request, catalog.downloadRequest(entry))
            // By index is the held feed's row, which has nothing to fetch —
            // exactly what a paging screen must not ask.
            assertNull(catalog.downloadRequest(entry.index))
        }
    }

    // ---- Completing ----

    @Test
    fun aFileTheAppFetchedItselfIsShelvedWithoutBeingConsumed() = withLibrary("import") { dir, library ->
        // The name a worker actually produces: an opaque temp file, no
        // extension anywhere. The format is read from the bytes, so it
        // shelves regardless.
        val source = handedOver("epub/minimal.epub", "download-3f9a1c", dir)
        val book = required(library.importFile(source.absolutePath), "the import answers with its library row")
        assertTrue(book > 0)

        val shelved = library.books()
        assertEquals(1, shelved.size)
        assertEquals(book, shelved.single().id)
        assertTrue(shelved.single().fingerprint.isNotEmpty())

        // The source belongs to whoever passed it — a worker's temp file, a
        // document-browser pick — so the library copies and never reaches
        // into the host's storage to clean up. `Catalog.download` removes
        // its own staging file because it made it; this is the opposite
        // case, and the difference is the whole distinction.
        assertTrue(source.exists())
        assertNotEquals(source.absolutePath, shelved.single().filePath)
    }

    @Test
    fun importingTheSameBytesTwiceAnswersWithTheSameRow() = withLibrary("import-retry") { dir, library ->
        // A different path and a different name, because a job system that
        // retries rarely lands the bytes in the same place twice.
        val first = handedOver("epub/minimal.epub", "attempt-1.epub", dir)
        val second = handedOver("epub/minimal.epub", "attempt-2", dir)

        val book = library.importFile(first.absolutePath)
        val again = library.importFile(second.absolutePath)

        // Books are identified by a fingerprint of their bytes, which is
        // what lets a worker the system restarted, or a completion
        // delivered twice, stay correct without coordinating with the shelf.
        assertNotNull(book)
        assertEquals("the same bytes are the same book", book, again)
        assertEquals("no duplicate row", 1, library.books().size)
    }

    @Test
    fun aFinishedDownloadIsShelvedThenGivenTheServicesItsFeedCarried() = withLibrary("import-sync") { dir, library ->
        // The whole flow, in the order the docs give it. The request is
        // built while the feed is open and the catalog is then closed —
        // by the time a background transfer lands, it usually is.
        val request = Catalog(FixtureCatalog()).use { catalog ->
            catalog.fetch(SYNCING_SHELF)
            required(catalog.downloadRequest(catalog.entries().first { it.canDownload }))
        }

        val source = handedOver("epub/minimal.epub", request.suggestedFilename, dir)
        val book = required(library.importFile(source.absolutePath))
        assertTrue(library.setSyncTargets(book, request.progressionUrl, request.annotationContainer))

        // Sync services are not in the file, so nothing but that call could
        // have put them there. A book that skipped it is one that will
        // never reconcile, which is the failure this ordering exists to
        // prevent.
        assertEquals("$ORIGIN/sync/position/v3", library.syncProgressionUrl(book))
        assertEquals("$ORIGIN/sync/annotations/v3", library.syncAnnotationContainer(book))
    }

    @Test
    fun importingSomethingThatIsNotABookFailsInsteadOfCrashing() = withLibrary("import-junk") { dir, library ->
        // A truncated transfer is an ordinary outcome for a background job,
        // and the host has to be able to tell "retry" from a crash.
        val junk = File(dir, "truncated.epub").apply { writeText("not a book") }
        assertNull(library.importFile(junk.absolutePath))
        assertTrue("nothing half-shelved", library.books().isEmpty())

        // A path with no file behind it is the other half of the same
        // question — a completion handed a file the system already
        // reclaimed.
        assertNull(library.importFile(File(dir, "reclaimed").absolutePath))
        assertTrue(library.books().isEmpty())
    }

    private companion object {
        const val ORIGIN = "http://catalog.test"
        const val ROOT = "$ORIGIN/opds/"
        const val SYNCING_SHELF = "$ORIGIN/opds/feed/sync"
        const val NAVIGATION = "application/atom+xml;profile=opds-catalog;kind=navigation"
        const val ACQUISITION = "application/atom+xml;profile=opds-catalog;kind=acquisition"
    }
}
