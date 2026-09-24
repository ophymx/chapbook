package com.ophymx.chapbook.app

import android.app.Application
import android.content.Context
import com.ophymx.chapbook.Session
import com.ophymx.chapbook.app.model.AppContainer

/**
 * The process. The engine has no voice until a host gives it one, so
 * logging is installed here, before anything can fail quietly.
 */
class ChapbookApplication : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        Session.initLogging(verbose = false)
        container = AppContainer(this)
    }
}

/** The application's model, from any context. */
val Context.container: AppContainer
    get() = (applicationContext as ChapbookApplication).container
