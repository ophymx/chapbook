package com.ophymx.chapbook.app

import android.net.Uri
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.ophymx.chapbook.BookQuery
import com.ophymx.chapbook.ReadingState
import com.ophymx.chapbook.Session
import com.ophymx.chapbook.app.model.Added
import com.ophymx.chapbook.app.model.Credentials
import com.ophymx.chapbook.app.model.Grants
import com.ophymx.chapbook.app.model.Http
import com.ophymx.chapbook.app.model.Opener
import com.ophymx.chapbook.app.model.Shelf
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * The model without a screen: a file comes in through the opener, shows
 * up on the shelf, opens for reading, and is found again.
 */
@RunWith(AndroidJUnit4::class)
class ShelfModelTest {

    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Before
    fun logging() = Session.initLogging()

    private fun fixture(path: String): ByteArray =
        InstrumentationRegistry.getInstrumentation().context.assets.open(path).use { it.readBytes() }

    private fun scratch(name: String): File =
        File(context.cacheDir, "$name-${System.nanoTime()}").also { check(it.mkdirs()) }

    private fun <T> withModel(name: String, body: (dir: File, shelf: Shelf, opener: Opener, grants: Grants) -> T): T {
        val dir = scratch(name)
        try {
            val credentials = Credentials(context)
            val shelf = Shelf(dir, credentials, Http(credentials).transport)
            return body(dir, shelf, Opener(context, shelf), Grants(shelf))
        } finally {
            dir.deleteRecursively()
        }
    }

    @Test
    fun aOneShotFileIsCopiedInAndShelvedAndOpens() = withModel("copy") { dir, shelf, opener, _ ->
        // A `file:` URI grants nothing persistable, which is the case a
        // viewer intent presents: the bytes are ours now and never again,
        // so the opener copies rather than adopts.
        val handed = File(dir, "handed.epub").apply { writeBytes(fixture("epub/minimal.epub")) }
        val added = runBlocking { opener.add(Uri.fromFile(handed)) }
        val id = (added as? Added.Book)?.id ?: error("not added: $added")

        val books = runBlocking { shelf.books(BookQuery()) }
        assertEquals(1, books.size)
        assertEquals(id, books.single().id)
        assertNotNull("the library holds its own copy", books.single().filePath)
        assertEquals(ReadingState.Unread, books.single().state)

        // The source is the platform's and untouched; the copy is what opens.
        assertTrue(handed.exists())
        val session = opener.open(books.single())
        assertNotNull(session)
        session!!.close()
    }

    @Test
    fun theSameBytesTwiceAreOneRow() = withModel("twice") { dir, shelf, opener, _ ->
        val a = File(dir, "a").apply { writeBytes(fixture("epub/minimal.epub")) }
        val b = File(dir, "b.epub").apply { writeBytes(fixture("epub/minimal.epub")) }
        val first = runBlocking { opener.add(Uri.fromFile(a)) } as Added.Book
        val second = runBlocking { opener.add(Uri.fromFile(b)) } as Added.Book
        assertEquals(first.id, second.id)
        assertEquals(1, runBlocking { shelf.books() }.size)
    }

    @Test
    fun junkIsRefusedAndTheShelfStaysClean() = withModel("junk") { dir, shelf, opener, _ ->
        val junk = File(dir, "notes.txt").apply { writeText("not a book") }
        assertTrue(runBlocking { opener.add(Uri.fromFile(junk)) } is Added.Failed)
        assertTrue(runBlocking { shelf.books() }.isEmpty())
    }

    @Test
    fun aGrantIsRememberedByFingerprintAndForgotten() = withModel("grants") { _, _, _, grants ->
        val fingerprint = "test-${System.nanoTime()}"
        val uri = Uri.parse("content://com.example.provider/document/42")
        assertNull(grants.uriFor(fingerprint))
        grants.remember(fingerprint, uri)
        assertEquals(uri, grants.uriFor(fingerprint))
        grants.forget(fingerprint)
        assertNull(grants.uriFor(fingerprint))
    }

    @Test
    fun anAdoptedBookKeepsNoCopyAndIsFoundAgainByItsGrant() = withModel("adopt") { dir, shelf, opener, grants ->
        // Adoption is the picker's door: a persistable grant, a record by
        // content, no copy. A `file:` URI grants nothing persistable, so
        // the opener's own door would import; adopt through the engine
        // directly, with the URI as the grant, the way the opener does
        // for a `content://` one.
        val file = File(dir, "picked.epub").apply { writeBytes(fixture("epub/minimal.epub")) }
        val uri = Uri.fromFile(file)
        val pfd = ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
        val id = checkNotNull(shelf.blockingApp { adoptFd(pfd, uri.toString().toByteArray()) }) { "not adopted" }
        val book = runBlocking { shelf.book(id) }!!
        assertNull("the platform owns the file", book.filePath)
        assertEquals(uri, grants.uriFor(book.fingerprint))
        assertTrue(file.exists())

        // Opening resolves the grant: a session over a descriptor.
        val session = opener.open(book)
        assertNotNull(session)
        assertTrue(session!!.title.isNotEmpty())
        session.close()

        // Then the grant is lost, or points nowhere, or the file is gone
        // from under it: the row survives and the reader is told the file
        // is out of reach rather than crashed.
        grants.forget(book.fingerprint)
        assertNull(opener.open(book))
        grants.remember(book.fingerprint, Uri.parse("content://nowhere/gone"))
        assertNull(opener.open(book))
        grants.remember(book.fingerprint, uri)
        assertTrue(file.delete())
        assertNull(opener.open(book))
    }
}
