package com.ophymx.chapbook.app.ui

import android.app.Application
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
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
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.BottomAppBar
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
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.ophymx.chapbook.Theme
import com.ophymx.chapbook.app.R
import com.ophymx.chapbook.app.container
import com.ophymx.chapbook.app.model.ReaderState
import com.ophymx.chapbook.app.model.ReaderViewModel
import kotlinx.coroutines.launch

/** Which sheet is up, if any. */
enum class Sheet { Contents, Search, Marks, Settings }

/** A tap on a stored highlight: which, and where on the page view. */
data class HighlightTap(val id: Long, val xPx: Float, val yPx: Float)

/** The page's ground, so the safe-inset strips match the engine's theme. */
@Composable
private fun paper(theme: Theme?): Color = when (theme) {
    Theme.LIGHT -> Color.White
    Theme.SEPIA -> Color(0xFFF6F0E2)
    Theme.DARK -> Color(0xFF121212)
    null -> MaterialTheme.colorScheme.background
}

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
    val settings by vm.settings.collectAsStateWithLifecycle()
    val fontFamily by vm.fontFamily.collectAsStateWithLifecycle()
    val marks by vm.marks.collectAsStateWithLifecycle()
    val search by vm.search.collectAsStateWithLifecycle()
    val selection by vm.selection.collectAsStateWithLifecycle()

    var chrome by remember { mutableStateOf(false) }
    var sheet by remember { mutableStateOf<Sheet?>(null) }
    var noteDialog by remember { mutableStateOf(false) }
    var highlightTap by remember { mutableStateOf<HighlightTap?>(null) }
    var selectionBounds by remember { mutableStateOf<SelectionBounds?>(null) }
    var failed by remember { mutableStateOf<Pair<Int, String>?>(null) }
    var unopenable by remember { mutableStateOf<String?>(null) }
    val snackbar = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    val copied = stringResource(R.string.copied)

    // Back peels one layer at a time: a sheet, then a selection, then the book.
    BackHandler {
        when {
            sheet != null -> sheet = null
            selection != null -> vm.clearSelection()
            chrome -> chrome = false
            else -> onBack()
        }
    }

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
    unopenable?.let { href ->
        val text = stringResource(R.string.link_failed, href)
        LaunchedEffect(href) {
            snackbar.showSnackbar(text)
            unopenable = null
        }
    }

    // The page never goes under the status bar, the camera cutout or the
    // gesture bar: the paper colour fills those, and the page box is
    // what is left. The chrome overlays the page, inside the same bounds.
    Box(
        Modifier
            .fillMaxSize()
            .background(paper(settings?.theme))
            .safeDrawingPadding(),
    ) {
        when (val s = state) {
            ReaderState.Opening -> CircularProgressIndicator(Modifier.align(Alignment.Center))
            ReaderState.Gone -> Column(
                Modifier.align(Alignment.Center).padding(32.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
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
                            onExternalLink = { href -> if (!openExternal(ctx, href)) unopenable = href }
                            onHighlightTapped = { id, x, y -> highlightTap = HighlightTap(id, x, y) }
                            onSelection = { bounds ->
                                selectionBounds = bounds
                                if (bounds == null) vm.clearSelection() else vm.selected(bounds.start, bounds.end)
                            }
                            page = this
                        }
                    },
                    modifier = Modifier.fillMaxSize(),
                )
                // Volume keys arrive at the activity, and the model's
                // changes want a repaint; both reach this page while it
                // is the one showing.
                DisposableEffect(page) {
                    val view = page
                    container.keys.onKeyDown = view?.let { v -> v::handleKey }
                    container.keys.bindsKey = view?.let { v -> v::bindsKey }
                    vm.onNeedsRedraw = view?.let { v -> { v.invalidate() } }
                    onDispose {
                        container.keys.onKeyDown = null
                        container.keys.bindsKey = null
                        vm.onNeedsRedraw = null
                    }
                }

                selectionBounds?.let { bounds ->
                    SelectionBar(
                        bounds = bounds,
                        onHighlight = vm::highlightSelection,
                        onNote = { noteDialog = true },
                        onCopy = {
                            val text = selection?.text ?: ""
                            context.getSystemService(ClipboardManager::class.java)
                                ?.setPrimaryClip(ClipData.newPlainText("chapbook", text))
                            vm.clearSelection()
                            scope.launch { snackbar.showSnackbar(copied) }
                        },
                    )
                }
                highlightTap?.let { tap ->
                    HighlightMenu(
                        tap = tap,
                        onColor = { color ->
                            vm.recolorHighlight(tap.id, color)
                            highlightTap = null
                        },
                        onRemove = {
                            vm.removeMark(tap.id)
                            highlightTap = null
                        },
                        onDismiss = { highlightTap = null },
                    )
                }

                when (sheet) {
                    Sheet.Contents -> ContentsSheet(
                        entries = s.toc,
                        currentSpine = place.spine,
                        onPick = {
                            vm.gotoToc(it)
                            sheet = null
                            chrome = false
                        },
                        onDismiss = { sheet = null },
                    )
                    Sheet.Search -> SearchSheet(
                        state = search,
                        onQuery = vm::search,
                        onPick = {
                            vm.gotoHit(it)
                            sheet = null
                            chrome = false
                        },
                        onDismiss = { sheet = null },
                    )
                    Sheet.Marks -> MarksSheet(
                        marks = marks,
                        onBookmark = vm::addBookmark,
                        onPick = {
                            vm.gotoMark(it.id)
                            sheet = null
                            chrome = false
                        },
                        onDelete = { vm.removeMark(it.id) },
                        onDismiss = { sheet = null },
                    )
                    Sheet.Settings -> SettingsSheet(
                        settings = settings,
                        fontFamily = fontFamily,
                        families = s.fontFamilies,
                        onSettings = vm::applySettings,
                        onFamily = vm::setFontFamily,
                        onReset = vm::resetBookSettings,
                        onDismiss = { sheet = null },
                    )
                    null -> {}
                }
                if (noteDialog) {
                    NoteDialog(
                        onSave = {
                            vm.noteOnSelection(it)
                            noteDialog = false
                        },
                        onDismiss = { noteDialog = false },
                    )
                }
            }
        }

        val bars = TopAppBarDefaults.topAppBarColors(
            containerColor = MaterialTheme.colorScheme.surface.copy(alpha = 0.94f),
        )
        AnimatedVisibility(visible = chrome, modifier = Modifier.align(Alignment.TopCenter)) {
            TopAppBar(
                colors = bars,
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
                actions = {
                    // The engine's Back: where the reader was before the
                    // last link, greyed out by absence rather than state.
                    if (place.canGoBack) {
                        TextButton(onClick = vm::goBack) { Text(stringResource(R.string.return_back)) }
                    }
                },
            )
        }
        AnimatedVisibility(visible = chrome, modifier = Modifier.align(Alignment.BottomCenter)) {
            BottomAppBar(containerColor = MaterialTheme.colorScheme.surface.copy(alpha = 0.94f)) {
                IconButton(onClick = { sheet = Sheet.Contents }) {
                    Icon(Icons.AutoMirrored.Filled.List, contentDescription = stringResource(R.string.contents))
                }
                IconButton(onClick = { sheet = Sheet.Search }) {
                    Icon(Icons.Default.Search, contentDescription = stringResource(R.string.search))
                }
                IconButton(onClick = { sheet = Sheet.Marks }) {
                    Icon(Icons.Default.Star, contentDescription = stringResource(R.string.marks))
                }
                IconButton(onClick = { sheet = Sheet.Settings }) {
                    Icon(Icons.Default.Settings, contentDescription = stringResource(R.string.settings))
                }
            }
        }
        SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter))
    }
}

/** An `http(s)` link the engine will not follow. False when no app takes it. */
private fun openExternal(context: Context, href: String): Boolean = try {
    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(href)))
    true
} catch (e: ActivityNotFoundException) {
    false
}
