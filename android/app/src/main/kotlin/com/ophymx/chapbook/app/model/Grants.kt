package com.ophymx.chapbook.app.model

import android.content.Context
import android.net.Uri
import androidx.core.content.edit

/**
 * How an adopted book is found again.
 *
 * A book opened from a `content://` URI reaches the library by content:
 * it is recorded under its fingerprint, and the library keeps no copy.
 * Reopening the *file* next launch is the app's job, and this is the
 * map that makes it possible — fingerprint to the persisted URI grant.
 * Keyed by fingerprint rather than row id because the fingerprint
 * survives a reinstall; the id is the reader's history of the book.
 */
class Grants(context: Context) {
    private val prefs = context.getSharedPreferences("grants", Context.MODE_PRIVATE)

    fun uriFor(fingerprint: String): Uri? =
        prefs.getString(fingerprint, null)?.let(Uri::parse)

    fun remember(fingerprint: String, uri: Uri) =
        prefs.edit { putString(fingerprint, uri.toString()) }

    fun forget(fingerprint: String) =
        prefs.edit { remove(fingerprint) }
}
