package com.ophymx.chapbook.app.model

import android.net.Uri

/**
 * How an adopted book is found again.
 *
 * A book opened from a `content://` URI reaches the library by content:
 * it is recorded under its fingerprint, and the library keeps no copy.
 * Reopening the *file* next launch is the app's job, and the engine
 * keeps the map that makes it possible — fingerprint to the persisted
 * URI grant, as opaque bytes beside the shelf. This is that map with the
 * bytes read as the URI they are. Blocking, like the opener that uses
 * it: never from the main thread.
 */
class Grants(private val shelf: Shelf) {
    fun uriFor(fingerprint: String): Uri? =
        shelf.blockingApp { grant(fingerprint) }?.let { Uri.parse(String(it, Charsets.UTF_8)) }

    fun remember(fingerprint: String, uri: Uri) {
        shelf.blockingApp { rememberGrant(fingerprint, uri.toString().toByteArray(Charsets.UTF_8)) }
    }

    fun forget(fingerprint: String) {
        shelf.blockingApp { forgetGrant(fingerprint) }
    }
}
