package com.ophymx.chapbook.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.clickable
import com.ophymx.chapbook.Annotation
import com.ophymx.chapbook.AnnotationKind
import com.ophymx.chapbook.ReadingSettings
import com.ophymx.chapbook.SearchHit
import com.ophymx.chapbook.Theme
import com.ophymx.chapbook.TocEntry
import com.ophymx.chapbook.app.R
import com.ophymx.chapbook.app.model.SearchState
import kotlinx.coroutines.delay
import kotlin.math.roundToInt

// ---- Contents ----

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ContentsSheet(entries: List<TocEntry>, currentSpine: Int, onPick: (TocEntry) -> Unit, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text(
            stringResource(R.string.contents),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
        )
        if (entries.isEmpty()) {
            Text(stringResource(R.string.contents_empty), Modifier.padding(24.dp))
        } else {
            LazyColumn {
                items(entries, key = { it.index }) { entry ->
                    // A heading that links nowhere is kept for its
                    // children's sake and drawn as one.
                    val linked = entry.spine != null
                    val current = entry.spine == currentSpine
                    Text(
                        entry.label,
                        style = MaterialTheme.typography.bodyLarge,
                        fontWeight = if (current) FontWeight.Bold else null,
                        color = if (linked) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable(enabled = linked) { onPick(entry) }
                            .padding(start = 24.dp + 16.dp * entry.depth, end = 24.dp, top = 12.dp, bottom = 12.dp),
                    )
                }
            }
        }
    }
}

// ---- Search ----

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SearchSheet(state: SearchState, onQuery: (String) -> Unit, onPick: (SearchHit) -> Unit, onDismiss: () -> Unit) {
    var query by remember { mutableStateOf(state.query) }
    val focus = remember { FocusRequester() }
    // A keystroke does not search; a pause does.
    LaunchedEffect(query) {
        delay(300)
        if (query.trim() != state.query) onQuery(query)
    }
    // Opening search means typing: the field takes focus and the
    // keyboard comes up, rather than the sheet's drag handle taking it.
    LaunchedEffect(Unit) { focus.requestFocus() }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            singleLine = true,
            placeholder = { Text(stringResource(R.string.search_hint)) },
            modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp).focusRequester(focus),
        )
        val summary = when {
            state.running -> stringResource(R.string.search_running)
            state.query.isEmpty() -> ""
            state.hits.isEmpty() -> stringResource(R.string.search_none)
            else -> stringResource(R.string.search_count, state.hits.size)
        }
        Text(summary, style = MaterialTheme.typography.labelMedium, modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp))
        LazyColumn {
            items(state.hits) { hit ->
                val text = remember(hit) {
                    val end = hit.matchEnd.coerceIn(0, hit.context.length)
                    val start = hit.matchStart.coerceIn(0, end)
                    AnnotatedString(
                        hit.context,
                        listOf(AnnotatedString.Range(SpanStyle(fontWeight = FontWeight.Bold), start, end)),
                    )
                }
                Text(
                    text,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.fillMaxWidth().clickable { onPick(hit) }.padding(horizontal = 24.dp, vertical = 10.dp),
                )
            }
        }
    }
}

// ---- Marks ----

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MarksSheet(
    marks: List<Annotation>,
    onBookmark: () -> Unit,
    onPick: (Annotation) -> Unit,
    onDelete: (Annotation) -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Button(onClick = onBookmark, modifier = Modifier.padding(horizontal = 24.dp)) {
            Text(stringResource(R.string.bookmark_page))
        }
        Spacer(Modifier.height(8.dp))
        if (marks.isEmpty()) {
            Text(stringResource(R.string.marks_empty), Modifier.padding(24.dp), style = MaterialTheme.typography.bodyMedium)
        } else {
            LazyColumn {
                items(marks, key = { it.id }) { mark ->
                    val kind = stringResource(
                        when (mark.kind) {
                            AnnotationKind.BOOKMARK -> R.string.mark_bookmark
                            AnnotationKind.HIGHLIGHT -> R.string.mark_highlight
                            AnnotationKind.NOTE -> R.string.mark_note
                        },
                    )
                    ListItem(
                        headlineContent = {
                            Text(mark.text?.takeIf { it.isNotBlank() } ?: kind, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        },
                        supportingContent = {
                            Text("$kind · " + stringResource(R.string.mark_at, (mark.progression * 100).roundToInt()))
                        },
                        trailingContent = {
                            IconButton(onClick = { onDelete(mark) }) {
                                Icon(Icons.Default.Delete, contentDescription = stringResource(R.string.delete))
                            }
                        },
                        modifier = Modifier.clickable { onPick(mark) },
                    )
                }
            }
        }
    }
}

// ---- Settings ----

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsSheet(
    settings: ReadingSettings?,
    fontFamily: String?,
    families: List<String>,
    onSettings: (ReadingSettings, thisBook: Boolean) -> Unit,
    onFamily: (String?, thisBook: Boolean) -> Unit,
    onReset: () -> Unit,
    onDismiss: () -> Unit,
) {
    var thisBook by remember { mutableStateOf(false) }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.settings), style = MaterialTheme.typography.titleMedium)
            if (settings == null) return@Column

            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.font_size), Modifier.width(120.dp))
                OutlinedButton(onClick = { onSettings(settings.copy(baseFontPx = (settings.baseFontPx - 2f).coerceAtLeast(10f)), thisBook) }) {
                    Text(stringResource(R.string.smaller))
                }
                Text("${settings.baseFontPx.roundToInt()}", Modifier.padding(horizontal = 12.dp))
                OutlinedButton(onClick = { onSettings(settings.copy(baseFontPx = (settings.baseFontPx + 2f).coerceAtMost(40f)), thisBook) }) {
                    Text(stringResource(R.string.larger))
                }
            }

            // The slider reflows on release, not on every pixel of the drag.
            var lineHeight by remember(settings.lineHeight) { mutableStateOf(settings.lineHeight) }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.line_height), Modifier.width(120.dp))
                Slider(
                    value = lineHeight,
                    onValueChange = { lineHeight = it },
                    onValueChangeFinished = { onSettings(settings.copy(lineHeight = lineHeight), thisBook) },
                    valueRange = 1f..2f,
                    steps = 9,
                    modifier = Modifier.weight(1f),
                )
                Text("%.1f".format(lineHeight), Modifier.width(36.dp))
            }

            ToggleRow(stringResource(R.string.justify), settings.justify) { onSettings(settings.copy(justify = it), thisBook) }
            ToggleRow(stringResource(R.string.publisher_styles), settings.publisherStyles) {
                onSettings(settings.copy(publisherStyles = it), thisBook)
            }

            Text(stringResource(R.string.theme), style = MaterialTheme.typography.labelLarge)
            SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                val themes = listOf(
                    Theme.LIGHT to R.string.theme_light,
                    Theme.SEPIA to R.string.theme_sepia,
                    Theme.DARK to R.string.theme_dark,
                )
                themes.forEachIndexed { i, (theme, label) ->
                    SegmentedButton(
                        selected = settings.theme == theme,
                        onClick = { onSettings(settings.copy(theme = theme), thisBook) },
                        shape = SegmentedButtonDefaults.itemShape(index = i, count = themes.size),
                    ) { Text(stringResource(label)) }
                }
            }

            Text(stringResource(R.string.typeface), style = MaterialTheme.typography.labelLarge)
            var picking by remember { mutableStateOf(false) }
            Box {
                OutlinedButton(onClick = { picking = true }) {
                    Text(fontFamily ?: stringResource(R.string.typeface_publisher))
                }
                DropdownMenu(expanded = picking, onDismissRequest = { picking = false }) {
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.typeface_publisher)) },
                        onClick = {
                            picking = false
                            onFamily(null, thisBook)
                        },
                    )
                    for (family in families) {
                        DropdownMenuItem(
                            text = { Text(family) },
                            onClick = {
                                picking = false
                                onFamily(family, thisBook)
                            },
                        )
                    }
                }
            }

            ToggleRow(stringResource(R.string.scope_this_book), thisBook) { thisBook = it }
            Text(stringResource(R.string.scope_hint), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton(onClick = onReset) { Text(stringResource(R.string.reset_book_settings)) }
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun ToggleRow(label: String, checked: Boolean, onChecked: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
        Text(label)
        Switch(checked = checked, onCheckedChange = onChecked)
    }
}

// ---- The selection's action bar ----

/** Floats above the selection, or below it when there is no room above. */
@Composable
fun SelectionBar(bounds: SelectionBounds, onHighlight: () -> Unit, onNote: () -> Unit, onCopy: () -> Unit) {
    val density = LocalDensity.current
    val barHeight = with(density) { 56.dp.roundToPx() }
    val gap = with(density) { 12.dp.roundToPx() }
    val handles = with(density) { 24.dp.roundToPx() }
    val above = bounds.boundsPx.top.roundToInt() - barHeight - gap
    val y = if (above >= 0) above else bounds.boundsPx.bottom.roundToInt() + handles + gap
    Box(Modifier.fillMaxWidth().offset { IntOffset(0, y) }, contentAlignment = Alignment.TopCenter) {
        Surface(shape = MaterialTheme.shapes.medium, tonalElevation = 6.dp, shadowElevation = 6.dp) {
            Row(Modifier.padding(horizontal = 4.dp)) {
                TextButton(onClick = onHighlight) { Text(stringResource(R.string.sel_highlight)) }
                TextButton(onClick = onNote) { Text(stringResource(R.string.sel_note)) }
                TextButton(onClick = onCopy) { Text(stringResource(R.string.sel_copy)) }
            }
        }
    }
}

// ---- A tapped highlight ----

private val highlightColors = listOf(
    R.string.color_yellow to "#ffe082",
    R.string.color_green to "#a5d6a7",
    R.string.color_blue to "#90caf9",
    R.string.color_pink to "#f48fb1",
)

@Composable
fun HighlightMenu(tap: HighlightTap, onColor: (String?) -> Unit, onRemove: () -> Unit, onDismiss: () -> Unit) {
    // A one-pixel anchor at the tap, so the menu opens where the finger was.
    Box(Modifier.offset { IntOffset(tap.xPx.roundToInt(), tap.yPx.roundToInt()) }.size(1.dp)) {
        DropdownMenu(expanded = true, onDismissRequest = onDismiss) {
            DropdownMenuItem(text = { Text(stringResource(R.string.color_theme)) }, onClick = { onColor(null) })
            for ((label, color) in highlightColors) {
                DropdownMenuItem(text = { Text(stringResource(label)) }, onClick = { onColor(color) })
            }
            DropdownMenuItem(text = { Text(stringResource(R.string.remove_highlight)) }, onClick = onRemove)
        }
    }
}

// ---- A note on the selection ----

@Composable
fun NoteDialog(onSave: (String) -> Unit, onDismiss: () -> Unit) {
    var body by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.note_title)) },
        text = {
            OutlinedTextField(
                value = body,
                onValueChange = { body = it },
                placeholder = { Text(stringResource(R.string.note_hint)) },
                minLines = 3,
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = {
            TextButton(onClick = { onSave(body) }, enabled = body.isNotBlank()) { Text(stringResource(R.string.save)) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
    )
}
