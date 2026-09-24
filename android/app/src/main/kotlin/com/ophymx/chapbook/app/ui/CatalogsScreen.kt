package com.ophymx.chapbook.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.ophymx.chapbook.app.R
import com.ophymx.chapbook.app.container
import com.ophymx.chapbook.app.model.CatalogsViewModel

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CatalogsScreen(onOpen: (String) -> Unit, onBack: () -> Unit) {
    val container = androidx.compose.ui.platform.LocalContext.current.container
    val vm: CatalogsViewModel = viewModel(factory = CatalogsViewModel.factory(container))
    val catalogs by vm.all.collectAsStateWithLifecycle()
    var adding by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.catalogs)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.back))
                    }
                },
            )
        },
        floatingActionButton = {
            FloatingActionButton(onClick = { adding = true }) {
                Icon(Icons.Default.Add, contentDescription = stringResource(R.string.catalog_add))
            }
        },
    ) { padding ->
        if (catalogs.isEmpty()) {
            Column(
                Modifier.fillMaxSize().padding(padding).padding(32.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(stringResource(R.string.catalogs_empty), style = MaterialTheme.typography.titleMedium)
                Text(
                    stringResource(R.string.catalogs_empty_hint),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else {
            LazyColumn(Modifier.padding(padding).fillMaxSize()) {
                items(catalogs, key = { it.id }) { catalog ->
                    ListItem(
                        headlineContent = { Text(catalog.title.ifBlank { catalog.url }, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                        supportingContent = { Text(catalog.url, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                        trailingContent = {
                            IconButton(onClick = { vm.remove(catalog.id) }) {
                                Icon(Icons.Default.Delete, contentDescription = stringResource(R.string.delete))
                            }
                        },
                        modifier = Modifier.clickable { onOpen(catalog.id) },
                    )
                }
            }
        }
    }

    if (adding) {
        var url by remember { mutableStateOf("") }
        AlertDialog(
            onDismissRequest = { adding = false },
            title = { Text(stringResource(R.string.catalog_add)) },
            text = {
                OutlinedTextField(
                    value = url,
                    onValueChange = { url = it },
                    singleLine = true,
                    label = { Text(stringResource(R.string.catalog_url)) },
                    placeholder = { Text(stringResource(R.string.catalog_url_hint)) },
                    modifier = Modifier.fillMaxWidth(),
                )
            },
            confirmButton = {
                TextButton(
                    enabled = url.isNotBlank(),
                    onClick = {
                        val added = vm.add(url)
                        adding = false
                        onOpen(added.id)
                    },
                ) { Text(stringResource(R.string.add)) }
            },
            dismissButton = { TextButton(onClick = { adding = false }) { Text(stringResource(R.string.cancel)) } },
        )
    }
}
