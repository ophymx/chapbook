package com.ophymx.chapbook.app

import android.app.Application
import android.content.Context
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import coil3.network.okhttp.OkHttpNetworkFetcherFactory
import com.ophymx.chapbook.Session
import com.ophymx.chapbook.app.model.AppContainer

/**
 * The process. The engine has no voice until a host gives it one, so
 * logging is installed here, before anything can fail quietly.
 */
class ChapbookApplication : Application(), SingletonImageLoader.Factory {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        Session.initLogging(verbose = false)
        container = AppContainer(this)
    }

    /** Covers load through the same client as everything else, credentials included. */
    override fun newImageLoader(context: PlatformContext): ImageLoader =
        ImageLoader.Builder(context)
            .components { add(OkHttpNetworkFetcherFactory(callFactory = { container.http.client })) }
            .build()
}

/** The application's model, from any context. */
val Context.container: AppContainer
    get() = (applicationContext as ChapbookApplication).container
