package com.ophymx.chapbook.app.model

import com.ophymx.chapbook.SyncResponse
import com.ophymx.chapbook.SyncTransport
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

/**
 * The app's networking: one client for the catalog, downloads and
 * covers.
 *
 * The binding bundles no HTTP stack, so the trust store is the device's
 * and a catalog behind a user CA or a corporate proxy works because the
 * platform's client does. Credentials are attached here, per request,
 * by the origin asked for — which is what lets a token rotated between
 * queueing a download and running it simply be fresh, and keeps the
 * secret out of anything WorkManager persists.
 */
class Http(private val credentials: Credentials) {
    val client: OkHttpClient = OkHttpClient.Builder()
        .followRedirects(true)
        // The engine's transport contract: no retries of its own.
        .retryOnConnectionFailure(false)
        .addInterceptor(Authorize())
        .build()

    /** The engine's transport, for catalogs and sync. */
    val transport: SyncTransport = OkHttpTransport(client)

    private inner class Authorize : Interceptor {
        override fun intercept(chain: Interceptor.Chain): okhttp3.Response {
            val request = chain.request()
            if (request.header("Authorization") != null) return chain.proceed(request)
            val credential = Credentials.originOf(request.url.toString())?.let(credentials::get)
                ?: return chain.proceed(request)
            return chain.proceed(request.newBuilder().header("Authorization", credential).build())
        }
    }
}

/**
 * [SyncTransport] over OkHttp, to the engine's contract: 4xx and 5xx
 * are responses, not exceptions; redirects followed; headers sent as
 * given; no retry; an exception only when no response came at all.
 */
class OkHttpTransport(private val client: OkHttpClient) : SyncTransport {
    override fun get(url: String, headers: Array<String>): SyncResponse = call("GET", url, headers, null)

    override fun send(method: String, url: String, headers: Array<String>, body: ByteArray): SyncResponse =
        call(method, url, headers, body)

    private fun call(method: String, url: String, headers: Array<String>, body: ByteArray?): SyncResponse {
        val builder = Request.Builder().url(url)
        var contentType: String? = null
        for (i in headers.indices step 2) {
            val name = headers[i]
            val value = headers.getOrNull(i + 1) ?: continue
            if (name.equals("Content-Type", ignoreCase = true)) contentType = value
            builder.header(name, value)
        }
        builder.method(method, body?.toRequestBody(contentType?.toMediaTypeOrNull()))
        client.newCall(builder.build()).execute().use { response ->
            // Every header, interleaved: `ETag` and `Location` are what the
            // annotation flows turn on, and there is no reason to guess
            // which others a server meant.
            val flat = ArrayList<String>(response.headers.size * 2)
            for ((name, value) in response.headers) {
                flat += name
                flat += value
            }
            return SyncResponse(response.code, response.header("Content-Type"), flat.toTypedArray(), response.body.bytes())
        }
    }
}
