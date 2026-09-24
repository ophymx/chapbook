package com.ophymx.chapbook.app.model

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

/** What the reader's progress readout says — the engine's choice set, the app's words. */
typealias ProgressLabel = com.ophymx.chapbook.ProgressLabel

/**
 * The app's display preferences, kept by the engine beside the shelf.
 *
 * These are the shell's, not the reader's: how the reader *shows* a
 * thing, not how it lays a page out. The value is read once at start
 * and mirrored here so a screen can collect it; a change is shown at
 * once and written behind the mirror.
 */
class Preferences(private val shelf: Shelf, private val scope: CoroutineScope) {
    private val _progressLabel = MutableStateFlow(ProgressLabel.PERCENT)
    val progressLabel: StateFlow<ProgressLabel> = _progressLabel

    init {
        scope.launch { _progressLabel.value = shelf.withApp { progressLabel } }
    }

    fun setProgressLabel(label: ProgressLabel) {
        _progressLabel.value = label
        scope.launch { shelf.withApp { progressLabel = label } }
    }
}
