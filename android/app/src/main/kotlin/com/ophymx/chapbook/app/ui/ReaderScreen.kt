package com.ophymx.chapbook.app.ui

import android.app.Application
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.ophymx.chapbook.app.R
import com.ophymx.chapbook.app.container
import com.ophymx.chapbook.app.model.ReaderState
import com.ophymx.chapbook.app.model.ReaderViewModel

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ReaderScreen(bookId: Long, onBack: () -> Unit) {
    val context = LocalContext.current
    val container = context.container
    val vm: ReaderViewModel = viewModel(
        key = "reader-$bookId",
        factory = ReaderViewModel.factory(context.applicationContext as Application, container, bookId),
    )
    val state by vm.state.collectAsStateWithLifecycle()
    val place by vm.place.collectAsStateWithLifecycle()
    var chrome by remember { mutableStateOf(false) }
    val snackbar = remember { SnackbarHostState() }
    var failed by remember { mutableStateOf<Pair<Int, String>?>(null) }

    BackHandler(onBack = onBack)

    // `onStop` is the last callback Android guarantees: save while there
    // is a process to save from.
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) vm.stopped()
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }

    failed?.let { (spine, message) ->
        val text = stringResource(R.string.page_failed, spine + 1, message)
        LaunchedEffect(spine, message) {
            snackbar.showSnackbar(text)
            failed = null
        }
    }

    // The page never goes under the status bar, the camera cutout or the
    // gesture bar: the app's background fills those, and the page box is
    // what is left. The chrome overlays the page, inside the same bounds.
    Box(
        Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .safeDrawingPadding(),
    ) {
        when (val s = state) {
            ReaderState.Opening -> CircularProgressIndicator(Modifier.align(Alignment.Center))
            ReaderState.Gone -> Column(Modifier.align(Alignment.Center).padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                Text(stringResource(R.string.open_gone), style = MaterialTheme.typography.bodyLarge)
                TextButton(onClick = onBack) { Text(stringResource(R.string.back)) }
            }
            is ReaderState.Reading -> {
                var page by remember { mutableStateOf<PageView?>(null) }
                AndroidView(
                    factory = { ctx ->
                        PageView(ctx, s.session).apply {
                            onMoved = vm::moved
                            onMenu = { chrome = !chrome }
                            onPageFailed = { spine, message -> failed = spine to message }
                            page = this
                        }
                    },
                    modifier = Modifier.fillMaxSize(),
                )
                // Volume keys arrive at the activity; route them here
                // while this page is the one showing.
                DisposableEffect(page) {
                    val view = page
                    container.keys.onKeyDown = view?.let { v -> v::handleKey }
                    container.keys.bindsKey = view?.let { v -> v::bindsKey }
                    onDispose {
                        container.keys.onKeyDown = null
                        container.keys.bindsKey = null
                    }
                }
            }
        }
        AnimatedVisibility(visible = chrome, modifier = Modifier.align(Alignment.TopCenter)) {
            TopAppBar(
                title = {
                    Column {
                        Text(place.title, style = MaterialTheme.typography.titleMedium, maxLines = 1)
                        Text(
                            stringResource(
                                R.string.reader_progress,
                                place.spine + 1,
                                place.spineLen,
                                place.page + 1,
                                place.pageCount,
                            ),
                            style = MaterialTheme.typography.labelSmall,
                        )
                    }
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.back))
                    }
                },
            )
        }
        SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter))
    }
}
