package com.ophymx.chapbook.app.ui

import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument

/** The shelf, a book, the saved catalogs, and one catalog being browsed. */
@Composable
fun ChapbookApp() {
    val nav = rememberNavController()
    NavHost(navController = nav, startDestination = "shelf") {
        composable("shelf") {
            ShelfScreen(
                onOpen = { id -> nav.navigate("reader/$id") { launchSingleTop = true } },
                onCatalogs = { nav.navigate("catalogs") },
            )
        }
        composable(
            "reader/{bookId}",
            arguments = listOf(navArgument("bookId") { type = NavType.LongType }),
        ) { entry ->
            val id = entry.arguments?.getLong("bookId") ?: return@composable
            ReaderScreen(bookId = id, onBack = { nav.popBackStack() })
        }
        composable("catalogs") {
            CatalogsScreen(
                onOpen = { id -> nav.navigate("catalog/$id") },
                onBack = { nav.popBackStack() },
            )
        }
        composable(
            "catalog/{catalogId}",
            arguments = listOf(navArgument("catalogId") { type = NavType.StringType }),
        ) { entry ->
            val id = entry.arguments?.getString("catalogId") ?: return@composable
            CatalogScreen(catalogId = id, onBack = { nav.popBackStack() })
        }
    }
}
