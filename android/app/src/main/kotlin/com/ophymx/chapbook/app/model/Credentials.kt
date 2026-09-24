package com.ophymx.chapbook.app.model

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.edit
import com.ophymx.chapbook.App
import com.ophymx.chapbook.CredentialStore
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The credentials the app holds, keyed the way the engine keys them.
 *
 * A credential is an opaque `Authorization` header value — a Basic
 * pair encoded, a bearer token, whatever the catalog took — and the key
 * is the engine's for the origin it is for ([App.credentialKey]), never
 * a catalog URL, whose path may itself be a secret. The engine stores a
 * sign-in through this class and reads it back before a fetch; the
 * app's own code — the cover loader, a download job — asks the same
 * way. Values sit in the Keystore's keeping: an AES key that never
 * leaves the hardware encrypts them, and preferences hold only
 * ciphertext. Nothing here prompts; a lookup is a lookup, on whatever
 * thread the engine is on.
 */
class Credentials(context: Context) : CredentialStore {
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

    /** The header value under [key], or null when none is held or it will not decrypt. */
    override fun get(key: String): String? {
        val stored = prefs.getString(key, null) ?: return null
        return try {
            val (iv, ciphertext) = stored.split(':', limit = 2).map { Base64.decode(it, Base64.NO_WRAP) }
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, this.key, GCMParameterSpec(128, iv))
            String(cipher.doFinal(ciphertext), Charsets.UTF_8)
        } catch (e: Exception) {
            null
        }
    }

    override fun set(key: String, value: String) {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, this.key)
        val ciphertext = cipher.doFinal(value.toByteArray(Charsets.UTF_8))
        val encoded = Base64.encodeToString(cipher.iv, Base64.NO_WRAP) + ":" +
            Base64.encodeToString(ciphertext, Base64.NO_WRAP)
        prefs.edit { putString(key, encoded) }
    }

    override fun forget(key: String) = prefs.edit { remove(key) }

    fun origins(): Set<String> = prefs.all.keys

    companion object {
        private const val ALIAS = "chapbook-credentials"

        /**
         * The key a credential for [url] lives under: the engine's, so a
         * sign-in the engine stored is what a cover request finds. Null
         * for anything that is not a URL with an origin.
         */
        fun originOf(url: String): String? = App.credentialKey(url)

        /** The Basic scheme's header value for a username and password. */
        fun basic(username: String, password: String): String = App.basicAuthorization(username, password)
    }
}
