package com.ophymx.chapbook.app.model

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory

/** The saved-catalogs screen: add, rename, remove. */
class CatalogsViewModel(private val catalogs: Catalogs) : ViewModel() {
    val all = catalogs.all

    fun add(url: String): SavedCatalog = catalogs.add(url)

    fun rename(id: String, title: String) = catalogs.rename(id, title)

    fun remove(id: String) = catalogs.remove(id)

    companion object {
        fun factory(container: AppContainer): ViewModelProvider.Factory = viewModelFactory {
            initializer { CatalogsViewModel(container.catalogs) }
        }
    }
}
