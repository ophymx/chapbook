package com.ophymx.chapbook.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.activity.compose.BackHandler
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import coil3.compose.AsyncImage
import androidx.compose.ui.layout.ContentScale
import com.ophymx.chapbook.CatalogEntry
import com.ophymx.chapbook.EntryKind
import com.ophymx.chapbook.Facet
import com.ophymx.chapbook.app.R
import com.ophymx.chapbook.app.container
import com.ophymx.chapbook.app.model.CatalogUi
import com.ophymx.chapbook.app.model.CatalogViewModel

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CatalogScreen(catalogId: String, onBack: () -> Unit) {
    val container = androidx.compose.ui.platform.LocalContext.current.container
    val saved = remember(catalogId) { container.catalogs.get(catalogId) }
    if (saved == null) {
        LaunchedEffect(Unit) { onBack() }
        return
    }
    val vm: CatalogViewModel = viewModel(
        key = "catalog-$catalogId",
        factory = CatalogViewModel.factory(container, saved),
    )
    val ui by vm.ui.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    var searching by remember { mutableStateOf(false) }

    // Back walks the catalog's own crumb trail before it leaves the screen.
    BackHandler { if (!vm.back()) onBack() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    val title = (ui as? CatalogUi.Feed)?.feed?.title ?: saved.title.ifBlank { saved.url }
                    Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                },
                navigationIcon = {
                    IconButton(onClick = { if (!vm.back()) onBack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.back))
                    }
                },
                actions = {
                    if ((ui as? CatalogUi.Feed)?.feed?.hasSearch == true) {
                        IconButton(onClick = { searching = true }) {
                            Icon(Icons.Default.Search, contentDescription = stringResource(R.string.search))
                        }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Box(Modifier.padding(padding).fillMaxSize()) {
            when (val state = ui) {
                CatalogUi.Opening -> CircularProgressIndicator(Modifier.align(Alignment.Center))
                is CatalogUi.Failed -> Column(
                    Modifier.align(Alignment.Center).padding(32.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text(stringResource(R.string.catalog_failed), style = MaterialTheme.typography.bodyLarge)
                    TextButton(onClick = onBack) { Text(stringResource(R.string.back)) }
                }
                is CatalogUi.Login -> LoginForm(
                    title = state.title,
                    offersBasic = state.offersBasic,
                    onSignIn = { u, p -> vm.signIn(u, p, state.retryUrl) },
                    onCancel = onBack,
                )
                is CatalogUi.Feed -> {
                    val feed = state.feed
                    if (feed.loading) {
                        CircularProgressIndicator(Modifier.align(Alignment.Center))
                    } else {
                        val downloadFailed = stringResource(R.string.download_failed, "")
                        FeedList(
                            entries = feed.entries,
                            facets = feed.facets,
                            loadingMore = feed.loadingMore,
                            hasMore = feed.nextPage != null,
                            onEntry = vm::openEntry,
                            onDownload = { entry ->
                                vm.download(entry) { }
                            },
                            onFacet = vm::applyFacet,
                            onLoadMore = vm::loadMore,
                        )
                    }
                }
            }
        }
    }

    if (searching) {
        var query by remember { mutableStateOf("") }
        AlertDialog(
            onDismissRequest = { searching = false },
            title = { Text(stringResource(R.string.search)) },
            text = {
                OutlinedTextField(
                    value = query,
                    onValueChange = { query = it },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            },
            confirmButton = {
                TextButton(enabled = query.isNotBlank(), onClick = {
                    vm.search(query)
                    searching = false
                }) { Text(stringResource(R.string.search)) }
            },
            dismissButton = { TextButton(onClick = { searching = false }) { Text(stringResource(R.string.cancel)) } },
        )
    }
}

@Composable
private fun FeedList(
    entries: List<CatalogEntry>,
    facets: List<Facet>,
    loadingMore: Boolean,
    hasMore: Boolean,
    onEntry: (CatalogEntry) -> Unit,
    onDownload: (CatalogEntry) -> Unit,
    onFacet: (Facet) -> Unit,
    onLoadMore: () -> Unit,
) {
    val listState = rememberLazyListState()
    // Reaching the end asks for the next page — infinite scroll.
    val atEnd by remember {
        derivedStateOf {
            val last = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            last >= listState.layoutInfo.totalItemsCount - 3
        }
    }
    LaunchedEffect(atEnd, hasMore) {
        if (atEnd && hasMore) onLoadMore()
    }

    LazyColumn(state = listState, modifier = Modifier.fillMaxSize()) {
        if (facets.isNotEmpty()) {
            item { FacetRow(facets, onFacet) }
        }
        items(entries, key = { it.index }) { entry ->
            EntryRow(entry, onEntry, onDownload)
        }
        if (loadingMore) {
            item {
                Row(Modifier.fillMaxWidth().padding(16.dp), horizontalArrangement = Arrangement.Center) {
                    CircularProgressIndicator()
                }
            }
        }
    }
}

@Composable
private fun FacetRow(facets: List<Facet>, onFacet: (Facet) -> Unit) {
    // One control per group; the facets of a group are alternatives.
    Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
        val groups = facets.groupBy { it.groupIndex }.toSortedMap()
        for ((_, groupFacets) in groups) {
            Text(
                groupFacets.first().group,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(start = 4.dp),
            )
            Row(
                Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(vertical = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                for (facet in groupFacets) {
                    FilterChip(
                        selected = facet.active,
                        onClick = { onFacet(facet) },
                        label = {
                            Text(facet.count?.let { "${facet.label} ($it)" } ?: facet.label)
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun EntryRow(entry: CatalogEntry, onEntry: (CatalogEntry) -> Unit, onDownload: (CatalogEntry) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(enabled = entry.kind == EntryKind.NAVIGATION) { onEntry(entry) }
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (entry.kind == EntryKind.PUBLICATION) {
            Box(
                Modifier.width(52.dp).aspectRatio(2f / 3f).align(Alignment.Top),
                contentAlignment = Alignment.Center,
            ) {
                if (entry.thumbnailUrl != null) {
                    AsyncImage(
                        model = entry.thumbnailUrl,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.fillMaxSize(),
                    )
                }
            }
            Spacer(Modifier.width(12.dp))
        }
        Column(Modifier.weight(1f)) {
            Text(entry.title, style = MaterialTheme.typography.bodyLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
            if (entry.authors.isNotEmpty()) {
                Text(
                    entry.authors.joinToString(", "),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            entry.summary?.takeIf { it.isNotBlank() && entry.kind == EntryKind.PUBLICATION }?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Spacer(Modifier.width(8.dp))
        when (entry.kind) {
            EntryKind.NAVIGATION -> Icon(
                Icons.AutoMirrored.Filled.KeyboardArrowRight,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            EntryKind.PUBLICATION -> if (entry.canDownload) {
                var queued by remember(entry.index) { mutableStateOf(false) }
                Button(enabled = !queued, onClick = {
                    queued = true
                    onDownload(entry)
                }) { Text(stringResource(R.string.get)) }
            }
        }
    }
}

@Composable
private fun LoginForm(title: String, offersBasic: Boolean, onSignIn: (String, String) -> Unit, onCancel: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(stringResource(R.string.sign_in_to, title), style = MaterialTheme.typography.titleMedium)
        if (!offersBasic) {
            Text(stringResource(R.string.sign_in_unavailable), color = MaterialTheme.colorScheme.error)
            TextButton(onClick = onCancel) { Text(stringResource(R.string.back)) }
            return
        }
        var username by remember { mutableStateOf("") }
        var password by remember { mutableStateOf("") }
        OutlinedTextField(
            value = username,
            onValueChange = { username = it },
            singleLine = true,
            label = { Text(stringResource(R.string.username)) },
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = password,
            onValueChange = { password = it },
            singleLine = true,
            visualTransformation = androidx.compose.ui.text.input.PasswordVisualTransformation(),
            label = { Text(stringResource(R.string.password)) },
            modifier = Modifier.fillMaxWidth(),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                enabled = username.isNotBlank(),
                onClick = { onSignIn(username, password) },
            ) { Text(stringResource(R.string.sign_in)) }
            TextButton(onClick = onCancel) { Text(stringResource(R.string.cancel)) }
        }
    }
}
