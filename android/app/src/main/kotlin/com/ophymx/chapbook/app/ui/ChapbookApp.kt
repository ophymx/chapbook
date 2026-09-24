package com.ophymx.chapbook.app.ui

import androidx.compose.runtime.Composable
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument

/** Two screens: the shelf, and a book. */
@Composable
fun ChapbookApp() {
    val nav = rememberNavController()
    NavHost(navController = nav, startDestination = "shelf") {
        composable("shelf") {
            ShelfScreen(onOpen = { id -> nav.navigate("reader/$id") { launchSingleTop = true } })
        }
        composable(
            "reader/{bookId}",
            arguments = listOf(navArgument("bookId") { type = NavType.LongType }),
        ) { entry ->
            val id = entry.arguments?.getLong("bookId") ?: return@composable
            ReaderScreen(bookId = id, onBack = { nav.popBackStack() })
        }
    }
}
