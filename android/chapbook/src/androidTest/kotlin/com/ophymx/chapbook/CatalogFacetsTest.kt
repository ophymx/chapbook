package com.ophymx.chapbook

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/** Facets cross grouped as the catalog groups them, with hrefs a host can fetch. */
@RunWith(AndroidJUnit4::class)
class CatalogFacetsTest {

    @Before
    fun logging() = Session.initLogging()

    private fun fixture(path: String): ByteArray =
        InstrumentationRegistry.getInstrumentation().context.assets.open(path).use { it.readBytes() }

    private inner class FixtureCatalog : SyncTransport {
        override fun get(url: String, headers: Array<String>): SyncResponse = when (url) {
            "$ORIGIN/opds/feed/new" -> SyncResponse(200, ACQUISITION, emptyArray(), fixture("opds/acquisition.atom.xml"))
            "$ORIGIN/opds/" -> SyncResponse(200, NAVIGATION, emptyArray(), fixture("opds/navigation.atom.xml"))
            else -> SyncResponse(404, "text/plain", emptyArray(), "no".toByteArray())
        }

        override fun send(method: String, url: String, headers: Array<String>, body: ByteArray): SyncResponse =
            throw UnsupportedOperationException("browsing never writes")
    }

    @Test
    fun facetsCrossGroupedAndResolved() {
        Catalog(FixtureCatalog()).use { catalog ->
            catalog.fetch("$ORIGIN/opds/feed/new")
            val facets = catalog.facets()
            assertEquals(listOf("English", "French", "EPUB"), facets.map { it.label })
            // Two groups, in feed order; the group index is what a screen
            // draws one control per.
            assertEquals(listOf("Language", "Language", "Format"), facets.map { it.group })
            assertEquals(listOf(0, 0, 1), facets.map { it.groupIndex })
            assertEquals(listOf(true, false, false), facets.map { it.active })
            assertEquals(listOf(80L, 40L, 100L), facets.map { it.count })
            // Root-relative in the fixture, absolute here: a host fetches it as is.
            assertEquals("$ORIGIN/opds/feed/new?lang=fr", facets[1].href)
            assertTrue(facets.all { it.index == facets.indexOf(it) })

            // A feed with none answers an empty list, not an error.
            catalog.fetch("$ORIGIN/opds/")
            assertTrue(catalog.facets().isEmpty())
            assertNull(Native.catalogFacetText(0, 0, 0))
        }
    }

    private companion object {
        const val ORIGIN = "http://catalog.test"
        const val NAVIGATION = "application/atom+xml;profile=opds-catalog;kind=navigation"
        const val ACQUISITION = "application/atom+xml;profile=opds-catalog;kind=acquisition"
    }
}
