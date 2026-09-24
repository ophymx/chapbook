package com.ophymx.chapbook.app.model

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.edit
import java.net.URI
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The credentials the app holds, keyed by origin.
 *
 * A credential is an opaque `Authorization` header value — a Basic
 * pair encoded, a bearer token, whatever the catalog took — and the key
 * is the origin it is for, never a catalog URL, whose path may itself
 * be a secret. Values sit in the Keystore's keeping: an AES key that
 * never leaves the hardware encrypts them, and preferences hold only
 * ciphertext. Nothing here prompts; a lookup is a lookup.
 */
class Credentials(context: Context) {
    private val prefs = context.getSharedPreferences("credentials", Context.MODE_PRIVATE)

    private val key: SecretKey by lazy {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey) ?: KeyGenerator
            .getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
            .apply {
                init(
                    KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                        .setKeySize(256)
                        .build(),
                )
            }
            .generateKey()
    }

    /** The header value for [origin], or null when none is held or it will not decrypt. */
    fun get(origin: String): String? {
        val stored = prefs.getString(origin, null) ?: return null
        return try {
            val (iv, ciphertext) = stored.split(':', limit = 2).map { Base64.decode(it, Base64.NO_WRAP) }
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, iv))
            String(cipher.doFinal(ciphertext), Charsets.UTF_8)
        } catch (e: Exception) {
            null
        }
    }

    fun set(origin: String, authorization: String) {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val ciphertext = cipher.doFinal(authorization.toByteArray(Charsets.UTF_8))
        val encoded = Base64.encodeToString(cipher.iv, Base64.NO_WRAP) + ":" +
            Base64.encodeToString(ciphertext, Base64.NO_WRAP)
        prefs.edit { putString(origin, encoded) }
    }

    fun forget(origin: String) = prefs.edit { remove(origin) }

    fun origins(): Set<String> = prefs.all.keys

    companion object {
        private const val ALIAS = "chapbook-credentials"

        /** `scheme://host[:port]`, lower-cased, default ports dropped — the key a credential lives under. */
        fun originOf(url: String): String? {
            val uri = try {
                URI(url)
            } catch (e: Exception) {
                return null
            }
            val scheme = uri.scheme?.lowercase() ?: return null
            val host = uri.host?.lowercase() ?: return null
            val port = uri.port
            val default = (scheme == "http" && port == 80) || (scheme == "https" && port == 443)
            return if (port == -1 || default) "$scheme://$host" else "$scheme://$host:$port"
        }

        /** The Basic scheme's header value for a username and password. */
        fun basic(username: String, password: String): String =
            "Basic " + Base64.encodeToString("$username:$password".toByteArray(Charsets.UTF_8), Base64.NO_WRAP)
    }
}
