package com.ophymx.chapbook.app.model

import android.content.Context
import android.net.Uri
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import java.io.File

/**
 * Everything the screens ask that is not a widget, built once per process.
 *
 * The decisions live in the engine's application layer (`chapbook-app`,
 * reached as [com.ophymx.chapbook.App]); what this package holds is the
 * platform's half — the Keystore behind the credential store, OkHttp
 * behind the transport, `WorkManager` behind a download, the content
 * resolver behind a grant — and the threads the engine's rules require.
 * Nothing in this package imports Compose, so all of it runs under an
 * instrumented test with no screen.
 */
class AppContainer(context: Context) {
    /** Where positions, marks, settings, grants, preferences and the library's copies live. */
    val libraryDir: File = context.filesDir

    /** Main-thread work the model starts on its own behalf: a list refreshed, a preference written. */
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    val credentials = Credentials(context)
    val http = Http(credentials)
    val shelf = Shelf(libraryDir, credentials, http.transport)
    val grants = Grants(shelf)
    val opener = Opener(context, shelf)
    val keys = KeyRouter()
    val catalogs = Catalogs(shelf, scope)
    val downloads = Downloads(context)
    val preferences = Preferences(shelf, scope)

    /**
     * A file another app asked us to open, waiting for a screen to take
     * it. Set from the activity's intent, cleared by whoever handles it.
     */
    val openRequests = MutableStateFlow<Uri?>(null)
}
