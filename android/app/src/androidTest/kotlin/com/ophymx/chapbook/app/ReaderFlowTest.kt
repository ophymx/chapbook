package com.ophymx.chapbook.app

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.ophymx.chapbook.AnnotationKind
import com.ophymx.chapbook.Locator
import com.ophymx.chapbook.Session
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * The reader chrome's flows, driven the way the sheets drive them:
 * a search walked unit by unit and a hit selected; a word under a point
 * becoming a highlight the marks list shows, recolours and removes; a
 * setting scoped to one book and forgotten again.
 */
@RunWith(AndroidJUnit4::class)
class ReaderFlowTest {

    @Before
    fun logging() = Session.initLogging()

    private fun fixture(path: String): ByteArray =
        InstrumentationRegistry.getInstrumentation().context.assets.open(path).use { it.readBytes() }

    private fun scratch(name: String): File =
        File(InstrumentationRegistry.getInstrumentation().targetContext.cacheDir, "$name-${System.nanoTime()}")
            .also { check(it.mkdirs()) }

    /** Metrics and one frame: nothing about a page is answerable before it is laid out. */
    private fun settle(session: Session) {
        session.setMetrics(360f, 640f, 20f, 1f)
        val size = checkNotNull(session.renderSize)
        val bitmap = Bitmap.createBitmap(size.width, size.height, Bitmap.Config.ARGB_8888)
        assertEquals(0, session.renderInto(bitmap))
        bitmap.recycle()
    }

    private fun <T> withBook(name: String, body: (Session) -> T): T {
        val dir = scratch(name)
        try {
            val book = File(dir, "book.epub").apply { writeBytes(fixture("epub/minimal.epub")) }
            return Session.open(book.absolutePath, dir.absolutePath)!!.use { session ->
                settle(session)
                body(session)
            }
        } finally {
            dir.deleteRecursively()
        }
    }

    @Test
    fun aSearchWalksTheBookUnitByUnitAndAHitCanBeShown() = withBook("search") { s ->
        // A word the page actually shows, so the hit is not a guess.
        val word = s.speakableText.split(Regex("\\s+"))
            .map { it.trim { c -> !c.isLetter() } }
            .first { it.length >= 4 }
        val hits = (0 until s.spineLen).flatMap { s.searchUnit(it, word) }
        assertTrue("found $word", hits.isNotEmpty())
        val hit = hits.first()
        assertTrue(hit.context.contains(word, ignoreCase = true))
        assertEquals(word.lowercase(), hit.context.substring(hit.matchStart, hit.matchEnd).lowercase())

        // Going to a hit and selecting it is what the results list does.
        s.goto(Locator(hit.spine, hit.start))
        s.selectRange(hit.start, hit.end)
        assertEquals(hit.start until hit.end, s.selectedRange)
        assertEquals(word.lowercase(), s.selectedText!!.trim().lowercase())
        assertTrue("the selection has geometry", s.rangeRects(hit.start, hit.end).isNotEmpty())
    }

    @Test
    fun aWordUnderTheFingerBecomesAHighlightTheMarksListKeeps() = withBook("marks") { s ->
        val run = s.pageTextRuns()!!.first { it.text.isNotBlank() }
        val x = run.rect.left + 4f
        val y = run.rect.centerY()
        assertTrue("a word is there", s.selectWordAt(x, y))
        val quote = s.selectedText!!
        assertTrue(quote.isNotBlank())

        val id = checkNotNull(s.addHighlight()) { "the selection became a highlight" }
        s.selectionClear()
        assertNull(s.selectedRange)

        val marks = s.annotations()
        assertEquals(1, marks.size)
        assertEquals(AnnotationKind.HIGHLIGHT, marks.single().kind)
        assertEquals(id, marks.single().id)
        assertEquals(quote.trim(), marks.single().text!!.trim())

        // The tap's second question: is a stored highlight under the finger?
        assertEquals(id, s.highlightAt(x, y))
        s.setHighlightColor(id, "#ffe082")
        assertEquals("#ffe082", s.annotations().single().color)

        assertNotNull(s.addBookmark())
        assertEquals(2, s.annotations().size)
        s.removeAnnotation(id)
        assertEquals(listOf(AnnotationKind.BOOKMARK), s.annotations().map { it.kind })
        assertNull(s.highlightAt(x, y))
    }

    @Test
    fun aSettingScopedToThisBookIsForgottenOnReset() = withBook("settings") { s ->
        val before = s.settings!!
        s.setSettings(before.copy(baseFontPx = before.baseFontPx + 6f), thisBook = true)
        assertEquals(before.baseFontPx + 6f, s.settings!!.baseFontPx)
        assertNotEquals(before, s.settings)
        s.clearBookSettings()
        assertEquals(before, s.settings)
    }
}
