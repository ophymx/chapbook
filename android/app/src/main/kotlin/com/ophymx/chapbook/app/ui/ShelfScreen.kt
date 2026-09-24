package com.ophymx.chapbook.app.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import coil3.compose.AsyncImage
import androidx.compose.ui.layout.ContentScale
import com.ophymx.chapbook.Book
import com.ophymx.chapbook.ReadingState
import com.ophymx.chapbook.Sort
import com.ophymx.chapbook.app.R
import com.ophymx.chapbook.app.container
import com.ophymx.chapbook.app.model.Notice
import com.ophymx.chapbook.app.model.ShelfViewModel
import java.io.File

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ShelfScreen(onOpen: (Long) -> Unit) {
    val container = androidx.compose.ui.platform.LocalContext.current.container
    val vm: ShelfViewModel = viewModel(factory = ShelfViewModel.factory(container))
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    val openFailed = stringResource(R.string.open_failed)

    // A file another app handed us. Taken here because the shelf is the
    // screen that can say what happened to it.
    val request by container.openRequests.collectAsStateWithLifecycle()
    LaunchedEffect(request) {
        val uri = request ?: return@LaunchedEffect
        container.openRequests.value = null
        vm.add(uri, onOpen)
    }

    // The picker. `OpenDocument` results can be persisted, which is what
    // lets the book be *adopted* rather than copied.
    val pick = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        // Every type, deliberately: providers report octet-stream for
        // perfectly good books, and the bytes decide the format.
        uri?.let { vm.add(it, onOpen) }
    }

    LaunchedEffect(state.notice) {
        when (state.notice) {
            Notice.OpenFailed -> {
                snackbar.showSnackbar(openFailed)
                vm.dismissNotice()
            }
            null -> {}
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_name)) },
                actions = { SortMenu(state.sort, vm::setSort) },
            )
        },
        floatingActionButton = {
            FloatingActionButton(onClick = { pick.launch(arrayOf("*/*")) }) {
                Icon(Icons.Default.Add, contentDescription = stringResource(R.string.shelf_add))
            }
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            OutlinedTextField(
                value = state.search,
                onValueChange = vm::setSearch,
                singleLine = true,
                placeholder = { Text(stringResource(R.string.shelf_search)) },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            )
            StateChips(state.state, vm::setStateFilter)
            when {
                state.loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator()
                }
                state.books.isEmpty() -> Empty(filtered = state.search.isNotBlank() || state.state != null)
                else -> LazyVerticalGrid(
                    columns = GridCells.Adaptive(minSize = 120.dp),
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 96.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                    modifier = Modifier.fillMaxSize(),
                ) {
                    items(state.books, key = { it.id }) { book ->
                        BookTile(
                            book = book,
                            onOpen = { onOpen(book.id) },
                            onFinished = { vm.setFinished(book, it) },
                            onRemove = { vm.remove(book) },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun SortMenu(current: Sort, onSort: (Sort) -> Unit) {
    var open by remember { mutableStateOf(false) }
    IconButton(onClick = { open = true }) {
        Icon(Icons.Default.MoreVert, contentDescription = stringResource(R.string.sort))
    }
    DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
        for ((sort, label) in listOf(
            Sort.Read to R.string.sort_read,
            Sort.Added to R.string.sort_added,
            Sort.Title to R.string.sort_title,
            Sort.Author to R.string.sort_author,
            Sort.Series to R.string.sort_series,
        )) {
            DropdownMenuItem(
                text = { Text(stringResource(label) + if (sort == current) "  ✓" else "") },
                onClick = {
                    open = false
                    onSort(sort)
                },
            )
        }
    }
}

@Composable
private fun StateChips(current: ReadingState?, onState: (ReadingState?) -> Unit) {
    Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        for ((state, label) in listOf(
            null to R.string.filter_all,
            ReadingState.Unread to R.string.filter_unread,
            ReadingState.Reading to R.string.filter_reading,
            ReadingState.Finished to R.string.filter_finished,
        )) {
            FilterChip(selected = state == current, onClick = { onState(state) }, label = { Text(stringResource(label)) })
        }
    }
}

@Composable
private fun Empty(filtered: Boolean) {
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            stringResource(if (filtered) R.string.shelf_no_match else R.string.shelf_empty),
            style = MaterialTheme.typography.titleMedium,
            textAlign = TextAlign.Center,
        )
        if (!filtered) {
            Spacer(Modifier.height(8.dp))
            Text(
                stringResource(R.string.shelf_empty_hint),
                style = MaterialTheme.typography.bodyMedium,
                textAlign = TextAlign.Center,
            )
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun BookTile(book: Book, onOpen: () -> Unit, onFinished: (Boolean) -> Unit, onRemove: () -> Unit) {
    var menu by remember { mutableStateOf(false) }
    var confirmRemove by remember { mutableStateOf(false) }
    Column(Modifier.combinedClickable(onClick = onOpen, onLongClick = { menu = true })) {
        Box(
            Modifier.fillMaxWidth().aspectRatio(2f / 3f).background(MaterialTheme.colorScheme.surfaceVariant),
            contentAlignment = Alignment.Center,
        ) {
            val cover = book.coverPath?.let(::File)
            if (cover != null && cover.exists()) {
                AsyncImage(
                    model = cover,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.fillMaxSize(),
                )
            } else {
                Text(
                    book.title,
                    style = MaterialTheme.typography.bodyMedium,
                    textAlign = TextAlign.Center,
                    maxLines = 4,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(8.dp),
                )
            }
        }
        book.progress?.takeIf { book.state == ReadingState.Reading }?.let {
            LinearProgressIndicator(progress = { it }, modifier = Modifier.fillMaxWidth())
        }
        Spacer(Modifier.height(6.dp))
        Text(book.title, style = MaterialTheme.typography.bodyMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
        Text(
            book.authors.joinToString(", "),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        Text(describeState(book), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            val finished = book.state == ReadingState.Finished
            DropdownMenuItem(
                text = { Text(stringResource(if (finished) R.string.mark_unread else R.string.mark_finished)) },
                onClick = {
                    menu = false
                    onFinished(!finished)
                },
            )
            DropdownMenuItem(
                text = { Text(stringResource(R.string.remove)) },
                onClick = {
                    menu = false
                    confirmRemove = true
                },
            )
        }
    }
    if (confirmRemove) {
        AlertDialog(
            onDismissRequest = { confirmRemove = false },
            title = { Text(stringResource(R.string.remove_title)) },
            text = { Text(stringResource(R.string.remove_body)) },
            confirmButton = {
                TextButton(onClick = {
                    confirmRemove = false
                    onRemove()
                }) { Text(stringResource(R.string.remove)) }
            },
            dismissButton = { TextButton(onClick = { confirmRemove = false }) { Text(stringResource(R.string.cancel)) } },
        )
    }
}

/** The state line, worded the way the CLI and the desktop app word it. */
@Composable
private fun describeState(book: Book): String = when (book.state) {
    ReadingState.Unread -> stringResource(R.string.state_unread)
    ReadingState.Reading -> stringResource(R.string.state_reading, ((book.progress ?: 0f) * 100).toInt())
    ReadingState.Finished -> stringResource(R.string.state_finished)
}
