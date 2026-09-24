package com.ophymx.chapbook

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * The two session calls a phone needs that a desktop shell never asks
 * for by name: a cache budget it chose itself, and a position saved
 * without leaving the book.
 */
@RunWith(AndroidJUnit4::class)
class SessionTest {

    @Before
    fun logging() = Session.initLogging()

    private fun fixture(path: String): ByteArray =
        InstrumentationRegistry.getInstrumentation().context.assets.open(path).use { it.readBytes() }

    private fun scratch(name: String): File =
        File(InstrumentationRegistry.getInstrumentation().targetContext.cacheDir, "$name-${System.nanoTime()}")
            .also { check(it.mkdirs()) }

    /**
     * Metrics and one frame. A restored position lands in the first
     * frame, not at open — the offset cannot become a page until the
     * unit has laid out — so a shell that reads `position` before it has
     * drawn sees page 0. The engine's own tests settle the same way.
     */
    private fun settle(session: Session) {
        session.setMetrics(200f, 300f, 16f, 1f)
        val size = checkNotNull(session.renderSize)
        val bitmap = Bitmap.createBitmap(size.width, size.height, Bitmap.Config.ARGB_8888)
        assertEquals("rendered", 0, session.renderInto(bitmap))
        bitmap.recycle()
    }

    private fun <T> withBook(name: String, body: (dir: File, book: File) -> T): T {
        val dir = scratch(name)
        try {
            val book = File(dir, "book.epub").apply { writeBytes(fixture("epub/minimal.epub")) }
            return body(dir, book)
        } finally {
            dir.deleteRecursively()
        }
    }

    @Test
    fun theCacheBudgetIsThePhonesToSet() = withBook("budget") { dir, book ->
        Session.open(book.absolutePath, dir.absolutePath)!!.use { session ->
            val default = session.cacheBudget
            assertTrue("the engine ships a default", default > 0)

            // A phone says its own number, and reads it back as said.
            session.cacheBudget = 16L * 1024 * 1024
            assertEquals(16L * 1024 * 1024, session.cacheBudget)
            assertNotEquals(default, session.cacheBudget)

            // Lowering from `onTrimMemory` must not touch the page on
            // screen: a zero budget still renders.
            session.setMetrics(300f, 500f, 16f, 1f)
            session.cacheBudget = 0
            assertEquals(0L, session.cacheBudget)
            assertTrue(session.pageCount > 0)
        }
    }

    @Test
    fun aPositionSavedMidBookIsWhereTheNextOpenLands() = withBook("save") { dir, book ->
        // The control: turning a page and closing saves nothing on its
        // own. Nothing in the engine persists on drop, so a shell that
        // forgets to call this loses the reader's place.
        Session.open(book.absolutePath, dir.absolutePath)!!.use { session ->
            settle(session)
            assertTrue(session.nextPage())
        }
        Session.open(book.absolutePath, dir.absolutePath)!!.use { session ->
            settle(session)
            assertEquals(Position(0, 0), session.position)
        }

        val moved = Session.open(book.absolutePath, dir.absolutePath)!!.use { session ->
            settle(session)
            assertTrue("the fixture has more than one page", session.nextPage())
            session.savePosition()
            session.position
        }
        assertNotEquals(Position(0, 0), moved)

        // No `suspend`, no `close`-time save: the explicit call alone is
        // what the next open finds. Metrics match so the page index means
        // the same thing.
        Session.open(book.absolutePath, dir.absolutePath)!!.use { session ->
            settle(session)
            assertEquals(moved, session.position)
        }
    }
}
