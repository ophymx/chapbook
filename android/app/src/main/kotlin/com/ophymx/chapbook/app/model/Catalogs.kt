package com.ophymx.chapbook.app.model

import android.content.Context
import androidx.core.content.edit
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

/** A catalog the reader added: where it is and what it called itself. */
data class SavedCatalog(val id: String, val title: String, val url: String)

/**
 * The catalogs the reader has added, in the order they were added.
 *
 * Nothing here is a secret — a catalog's credential lives in
 * [Credentials] under its origin — so plain preferences hold the list.
 * The engine has no notion of a saved catalog; the shelf only knows
 * the books that came from one.
 */
class Catalogs(context: Context) {
    private val prefs = context.getSharedPreferences("catalogs", Context.MODE_PRIVATE)

    private val _all = MutableStateFlow(load())
    val all: StateFlow<List<SavedCatalog>> = _all

    fun get(id: String): SavedCatalog? = _all.value.firstOrNull { it.id == id }

    /** Add a catalog; [title] may be blank until its feed says what it is called. */
    fun add(url: String, title: String = ""): SavedCatalog {
        val catalog = SavedCatalog(UUID.randomUUID().toString(), title.trim(), url.trim())
        save(_all.value + catalog)
        return catalog
    }

    fun rename(id: String, title: String) =
        save(_all.value.map { if (it.id == id) it.copy(title = title) else it })

    fun remove(id: String) = save(_all.value.filterNot { it.id == id })

    private fun save(list: List<SavedCatalog>) {
        val array = JSONArray()
        for (c in list) {
            array.put(JSONObject().put("id", c.id).put("title", c.title).put("url", c.url))
        }
        prefs.edit { putString("list", array.toString()) }
        _all.value = list
    }

    private fun load(): List<SavedCatalog> {
        val raw = prefs.getString("list", null) ?: return emptyList()
        return try {
            val array = JSONArray(raw)
            (0 until array.length()).map { i ->
                val o = array.getJSONObject(i)
                SavedCatalog(o.getString("id"), o.optString("title"), o.getString("url"))
            }
        } catch (e: Exception) {
            emptyList()
        }
    }
}
