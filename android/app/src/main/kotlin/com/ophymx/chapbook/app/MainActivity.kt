package com.ophymx.chapbook.app

import android.content.Intent
import android.os.Bundle
import android.view.KeyEvent
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import com.ophymx.chapbook.app.ui.ChapbookApp
import com.ophymx.chapbook.app.ui.ChapbookTheme

class MainActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent { ChapbookTheme { ChapbookApp() } }
        if (savedInstanceState == null) offer(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        offer(intent)
    }

    /** A file another app wants read. The shelf takes it from here. */
    private fun offer(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW) return
        intent.data?.let { container.openRequests.value = it }
    }

    // Volume keys arrive here and belong to whichever page is showing.
    // The engine's answer to "was that ours" is what has to be returned:
    // "did anything move" would put the volume slider over the last page.
    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean =
        container.keys.onKeyDown?.invoke(keyCode) == true || super.onKeyDown(keyCode, event)

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean =
        container.keys.bindsKey?.invoke(keyCode) == true || super.onKeyUp(keyCode, event)
}
