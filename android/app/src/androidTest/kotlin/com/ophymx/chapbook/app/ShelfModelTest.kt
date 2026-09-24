package com.ophymx.chapbook.app

import android.net.Uri
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.ophymx.chapbook.BookQuery
import com.ophymx.chapbook.ReadingState
import com.ophymx.chapbook.Session
import com.ophymx.chapbook.app.model.Added
import com.ophymx.chapbook.app.model.Grants
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
            val shelf = Shelf(dir)
            val grants = Grants(context)
            return body(dir, shelf, Opener(context, dir, shelf, grants), grants)
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
    fun aBookWhoseFileIsGoneDoesNotOpen() = withModel("gone") { dir, shelf, opener, grants ->
        // Adopted, then the grant is lost: the shelf row survives and the
        // reader is told the file is out of reach rather than crashed.
        val copy = File(dir, "adopted.epub").apply { writeBytes(fixture("epub/minimal.epub")) }
        val id = (runBlocking { opener.add(Uri.fromFile(copy)) } as Added.Book).id
        val book = runBlocking { shelf.book(id) }!!
        // Simulate the adopted shape: no copy, only a grant — and a grant
        // that no longer resolves.
        val adopted = book.copy(filePath = null)
        grants.forget(adopted.fingerprint)
        assertNull(opener.open(adopted))
        grants.remember(adopted.fingerprint, Uri.parse("content://nowhere/gone"))
        assertNull(opener.open(adopted))
        grants.forget(adopted.fingerprint)
    }
}
