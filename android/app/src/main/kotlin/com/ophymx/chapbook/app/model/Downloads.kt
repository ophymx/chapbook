package com.ophymx.chapbook.app.model

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.ForegroundInfo
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import com.ophymx.chapbook.DownloadRequest
import com.ophymx.chapbook.app.R
import com.ophymx.chapbook.app.container
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.withContext
import okhttp3.Request
import java.io.File
import java.io.IOException
import java.util.concurrent.TimeUnit

/**
 * Downloads as jobs, the way a transfer that has to outlive its screen
 * runs on a phone.
 *
 * The catalog describes a fetch ([DownloadRequest]) and steps aside; the
 * request goes into WorkManager's input — everything but a credential,
 * which the worker reads from [Credentials] by origin when it runs, so
 * no secret sits in WorkManager's database and a token rotated meanwhile
 * is simply fresh. When the file lands the worker hands it to the shelf
 * and records the sync services the entry carried, which live in the
 * entry and nowhere else — the reason the request captured them before
 * the transfer rather than after.
 */
class Downloads(context: Context) {
    private val manager = WorkManager.getInstance(context)

    fun enqueue(request: DownloadRequest) {
        val data = workDataOf(
            DownloadWorker.KEY_URL to request.url,
            DownloadWorker.KEY_NAME to request.suggestedFilename,
            DownloadWorker.KEY_TITLE to request.title,
            DownloadWorker.KEY_ENTRY to request.entryId,
            DownloadWorker.KEY_MEDIA to request.mediaType,
            DownloadWorker.KEY_PROGRESSION to request.progressionUrl,
            DownloadWorker.KEY_CONTAINER to request.annotationContainer,
        )
        val work = OneTimeWorkRequestBuilder<DownloadWorker>()
            .setInputData(data)
            .addTag(TAG)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
            .build()
        // One job per entry: tapping twice does not fetch twice, and a
        // job system that retries meets an import that is idempotent.
        manager.enqueueUniqueWork("download:${request.entryId}", ExistingWorkPolicy.KEEP, work)
    }

    /** Every download job, live: what a shelf shows as arriving. */
    fun status(): Flow<List<WorkInfo>> = manager.getWorkInfosByTagFlow(TAG)

    companion object {
        const val TAG = "download"
    }
}

/** The job: fetch to a staging file, shelve it, record where it syncs. */
class DownloadWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        val url = inputData.getString(KEY_URL) ?: return Result.failure()
        val title = inputData.getString(KEY_TITLE).orEmpty().ifBlank { inputData.getString(KEY_NAME).orEmpty() }
        val notifications = Notifications(applicationContext)
        setForeground(notifications.foreground(id.hashCode(), title, 0))

        val container = applicationContext.container
        val staged = File(applicationContext.cacheDir, "download-$id")
        try {
            val fetched = withContext(Dispatchers.IO) { fetch(url, staged) { percent -> setProgressAsync(workDataOf(KEY_PERCENT to percent)) } }
            when (fetched) {
                Fetched.Ok -> {}
                Fetched.Refused -> return Result.failure(workDataOf(KEY_ERROR to "refused"))
                Fetched.Gone -> return Result.failure(workDataOf(KEY_ERROR to "gone"))
                Fetched.Again -> return Result.retry()
            }
            // The import copies; the source is ours and goes in `finally`.
            val book = container.shelf.importFile(staged.absolutePath)
                ?: return Result.failure(workDataOf(KEY_ERROR to "not a book"))
            container.shelf.setSyncTargets(book, inputData.getString(KEY_PROGRESSION), inputData.getString(KEY_CONTAINER))
            notifications.done(id.hashCode(), title)
            return Result.success(workDataOf(KEY_BOOK to book))
        } catch (e: IOException) {
            return Result.retry()
        } finally {
            staged.delete()
        }
    }

    private enum class Fetched { Ok, Refused, Gone, Again }

    private fun fetch(url: String, into: File, progress: (Int) -> Unit): Fetched {
        // `Authorization` is the client's to add, by origin, at this moment.
        val request = Request.Builder().url(url).header("Accept", "*/*").build()
        applicationContext.container.http.client.newCall(request).execute().use { response ->
            when {
                response.code == 401 || response.code == 403 -> return Fetched.Refused
                response.code == 404 || response.code == 410 -> return Fetched.Gone
                response.code >= 500 -> return Fetched.Again
                !response.isSuccessful -> return Fetched.Gone
            }
            val total = response.body.contentLength()
            var seen = 0L
            var lastPercent = -1
            response.body.byteStream().use { input ->
                into.outputStream().use { out ->
                    val buffer = ByteArray(64 * 1024)
                    while (true) {
                        val n = input.read(buffer)
                        if (n < 0) break
                        out.write(buffer, 0, n)
                        seen += n
                        if (total > 0) {
                            val percent = (seen * 100 / total).toInt()
                            if (percent != lastPercent && percent % 5 == 0) {
                                lastPercent = percent
                                progress(percent)
                            }
                        }
                    }
                }
            }
        }
        return Fetched.Ok
    }

    companion object {
        const val KEY_URL = "url"
        const val KEY_NAME = "name"
        const val KEY_TITLE = "title"
        const val KEY_ENTRY = "entry"
        const val KEY_MEDIA = "media"
        const val KEY_PROGRESSION = "progression"
        const val KEY_CONTAINER = "container"
        const val KEY_PERCENT = "percent"
        const val KEY_BOOK = "book"
        const val KEY_ERROR = "error"
    }
}

/** The download channel and its two notifications: arriving, and arrived. */
class Notifications(private val context: Context) {
    private val manager = context.getSystemService(NotificationManager::class.java)

    init {
        manager?.createNotificationChannel(
            NotificationChannel(CHANNEL, context.getString(R.string.downloads), NotificationManager.IMPORTANCE_LOW),
        )
    }

    private fun open(): PendingIntent = PendingIntent.getActivity(
        context,
        0,
        Intent(context, Class.forName("com.ophymx.chapbook.app.MainActivity")),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    fun foreground(id: Int, title: String, percent: Int): ForegroundInfo {
        val notification: Notification = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle(context.getString(R.string.downloading, title))
            .setProgress(100, percent, percent == 0)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .build()
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            ForegroundInfo(id, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            ForegroundInfo(id, notification)
        }
    }

    fun done(id: Int, title: String) {
        val notification = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle(context.getString(R.string.downloaded, title))
            .setContentIntent(open())
            .setAutoCancel(true)
            .build()
        try {
            manager?.notify(id, notification)
        } catch (e: SecurityException) {
            // No notification permission: the book is on the shelf regardless.
        }
    }

    companion object {
        const val CHANNEL = "downloads"
    }
}
