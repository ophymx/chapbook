package com.ophymx.chapbook.app.model

import android.content.Context
import android.net.Uri
import kotlinx.coroutines.flow.MutableStateFlow
import java.io.File

/**
 * Everything the screens ask that is not a widget, built once per process.
 *
 * This is the app's model in the sense `chapbook-app` is the desktop's:
 * which books the shelf shows, how a book is opened and how it is found
 * again, and the threads the engine's rules require. Nothing in this
 * package imports Compose, so all of it runs under an instrumented test
 * with no screen.
 */
class AppContainer(context: Context) {
    /** Where positions, marks, settings and the library's copies live. */
    val libraryDir: File = context.filesDir

    val shelf = Shelf(libraryDir)
    val grants = Grants(context)
    val opener = Opener(context, libraryDir, shelf, grants)
    val keys = KeyRouter()
    val credentials = Credentials(context)
    val http = Http(credentials)
    val catalogs = Catalogs(context)
    val downloads = Downloads(context)
    val preferences = Preferences(context)

    /**
     * A file another app asked us to open, waiting for a screen to take
     * it. Set from the activity's intent, cleared by whoever handles it.
     */
    val openRequests = MutableStateFlow<Uri?>(null)
}
