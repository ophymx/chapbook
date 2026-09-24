package com.ophymx.chapbook.app.model

import android.content.Context
import android.content.Intent
import android.net.Uri
import com.ophymx.chapbook.Book
import com.ophymx.chapbook.Session
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File

/** What adding a file came to. */
sealed class Added {
    /** On the shelf, under this row. */
    data class Book(val id: Long) : Added()

    /** Not a book, or not reachable. [reason] is for logcat, not the reader. */
    data class Failed(val reason: String) : Added()
}

/**
 * The two doors a book comes in through, and the one it goes out to.
 *
 * **Custody follows the grant.** A URI whose read grant persists is
 * *adopted*: the library records the book by content and keeps no copy,
 * because the file is the platform's and a copy would be a second one
 * to keep in step. A URI whose grant is one-shot — a share, a viewer
 * intent — is *imported*, a copy made while the bytes are still ours,
 * because there is no way to reach the file again. Both land on the
 * same shelf; only [Book.filePath] tells them apart.
 */
class Opener(
    context: Context,
    private val libraryDir: File,
    private val shelf: Shelf,
    private val grants: Grants,
) {
    private val resolver = context.contentResolver
    private val cacheDir = context.cacheDir
    private val libraryPath = libraryDir.absolutePath

    /** A picked or shared file. Blocking work happens off the caller's thread. */
    suspend fun add(uri: Uri): Added = withContext(Dispatchers.IO) {
        if (takePersistable(uri)) adopt(uri) else import(uri)
    }

    private fun takePersistable(uri: Uri): Boolean = try {
        resolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        true
    } catch (e: SecurityException) {
        false
    }

    private suspend fun adopt(uri: Uri): Added {
        val pfd = try {
            resolver.openFileDescriptor(uri, "r")
        } catch (e: Exception) {
            null
        } ?: return Added.Failed("no descriptor for $uri")
        // Opening is what records the book; the session itself is not
        // wanted yet. Nothing is saved on close, so this leaves no mark.
        val id = (Session.openFd(pfd, libraryPath) ?: return Added.Failed("$uri is not a book"))
            .use { it.bookId() }
            ?: return Added.Failed("$uri did not reach the shelf")
        shelf.book(id)?.let { grants.remember(it.fingerprint, uri) }
        return Added.Book(id)
    }

    private suspend fun import(uri: Uri): Added {
        val staged = File(cacheDir, "import-${System.nanoTime()}")
        try {
            val input = try {
                resolver.openInputStream(uri)
            } catch (e: Exception) {
                null
            } ?: return Added.Failed("could not read $uri")
            input.use { from -> staged.outputStream().use(from::copyTo) }
            val id = shelf.importFile(staged.absolutePath) ?: return Added.Failed("$uri is not a book")
            return Added.Book(id)
        } finally {
            staged.delete()
        }
    }

    /**
     * Open a shelf row for reading, or null when its file is out of
     * reach: a grant the platform revoked, a copy the reader deleted.
     * Blocking; call off the main thread.
     */
    fun open(book: Book): Session? {
        book.filePath?.let { return Session.open(it, libraryPath) }
        val uri = grants.uriFor(book.fingerprint) ?: return null
        val pfd = try {
            resolver.openFileDescriptor(uri, "r")
        } catch (e: Exception) {
            null
        } ?: return null
        return Session.openFd(pfd, libraryPath)
    }
}
