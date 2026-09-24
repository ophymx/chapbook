package com.ophymx.chapbook.app

import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.ophymx.chapbook.app.model.Credentials
import com.ophymx.chapbook.app.model.Http
import com.ophymx.chapbook.app.model.OkHttpTransport
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The networking model without a screen: origins parsed the way a
 * credential is keyed, credentials kept in the Keystore's cipher, and the
 * transport meeting the engine's contract against a real socket.
 */
@RunWith(AndroidJUnit4::class)
class CatalogModelTest {

    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun aCredentialKeyIsTheEnginesAndDropsThePath() {
        // The engine's key, so a sign-in the engine stored is what a
        // cover request finds: the origin, lower-cased, never the path.
        assertEquals("opds/origin/https://example.org", Credentials.originOf("https://Example.org/opds/feed?x=1"))
        assertEquals("opds/origin/https://example.org:8443", Credentials.originOf("https://example.org:8443/x"))
        assertEquals("opds/origin/http://host:8080", Credentials.originOf("http://user:pw@host:8080/feed"))
        assertNull(Credentials.originOf("not a url"))
        assertNull(Credentials.originOf("/books/a.epub"))
    }

    @Test
    fun aCredentialSurvivesTheKeystoreRoundTripAndIsForgotten() {
        val creds = Credentials(context)
        val origin = "https://cred-test-${System.nanoTime()}.example.org"
        assertNull(creds.get(origin))
        val header = Credentials.basic("reader", "secret")
        assertEquals("Basic " + Base64.encodeToString("reader:secret".toByteArray(), Base64.NO_WRAP), header)
        creds.set(origin, header)
        assertEquals(header, creds.get(origin))
        assertTrue(creds.origins().contains(origin))
        creds.forget(origin)
        assertNull(creds.get(origin))
    }

    @Test
    fun theTransportAttachesTheOriginsCredentialAndSpeaksTheContract() {
        val server = MockWebServer()
        server.start()
        try {
            val base = server.url("/").toString().trimEnd('/')
            val origin = Credentials.originOf(base)!!
            val creds = Credentials(context)
            creds.set(origin, "Bearer token-42")
            val http = Http(creds)

            // A GET carries the origin's credential, added by the client.
            server.enqueue(MockResponse(code = 200, body = "feed"))
            val get = http.transport.get("$base/opds/", emptyArray())
            assertEquals(200, get.status)
            assertEquals("feed", String(get.body))
            val first = server.takeRequest()
            assertEquals("Bearer token-42", first.headers["Authorization"])

            // 4xx is a response, not an exception; headers cross interleaved.
            server.enqueue(MockResponse.Builder().code(404).addHeader("ETag", "\"v1\"").body("no").build())
            val missing = http.transport.get("$base/gone", emptyArray())
            assertEquals(404, missing.status)
            val headers = missing.headers.toList().chunked(2).associate { it[0] to it[1] }
            assertEquals("\"v1\"", headers["ETag"])
            server.takeRequest() // drain the 404 so the next take is the PUT

            // send() carries a body and method through unchanged.
            server.enqueue(MockResponse(code = 201))
            val put = http.transport.send("PUT", "$base/pos", arrayOf("Content-Type", "application/json"), "{}".toByteArray())
            assertEquals(201, put.status)
            val third = server.takeRequest()
            assertEquals("PUT", third.method)
        } finally {
            server.close()
        }
    }
}
