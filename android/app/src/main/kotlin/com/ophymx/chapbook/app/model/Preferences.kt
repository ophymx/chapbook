package com.ophymx.chapbook.app.model

import android.content.Context
import androidx.core.content.edit
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/** What the reader's progress readout says, beside the whole-book bar. */
enum class ProgressLabel {
    /** "34%" of the whole book. */
    PERCENT,

    /** "6 left in chapter" — how many pages remain in this unit. */
    PAGES_LEFT,

    /** "unit 6/20 · page 2/11" — the raw indices. */
    CHAPTER_PAGE,
}

/**
 * The app's display preferences.
 *
 * These are the shell's, not the engine's: how the reader *shows* a
 * thing, not how it lays a page out. So they live in plain preferences
 * here rather than in `ReadingSettings`, which crosses the engine
 * boundary and drives layout. Nothing here is a secret.
 */
class Preferences(context: Context) {
    private val prefs = context.getSharedPreferences("preferences", Context.MODE_PRIVATE)

    private val _progressLabel = MutableStateFlow(readProgressLabel())
    val progressLabel: StateFlow<ProgressLabel> = _progressLabel

    fun setProgressLabel(label: ProgressLabel) {
        prefs.edit { putString(KEY_PROGRESS, label.name) }
        _progressLabel.value = label
    }

    private fun readProgressLabel(): ProgressLabel =
        prefs.getString(KEY_PROGRESS, null)?.let { name ->
            runCatching { ProgressLabel.valueOf(name) }.getOrNull()
        } ?: ProgressLabel.PERCENT

    private companion object {
        const val KEY_PROGRESS = "progress_label"
    }
}
