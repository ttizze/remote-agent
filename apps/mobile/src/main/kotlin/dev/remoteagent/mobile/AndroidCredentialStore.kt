package dev.remoteagent.mobile

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import java.io.ByteArrayOutputStream
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Android Keystore seals the raw 32-byte iroh identity with a non-exportable AES-GCM key. */
internal class AndroidCredentialStore(context: Context, reference: String) {
    private val identifier = stableIdentifier(reference)
    private val keyStoreAlias = "remote-agent.iroh.identity.$identifier"
    private val encryptedFile = AtomicFile(File(context.filesDir, "iroh-identity-$identifier"))

    @Synchronized
    fun loadOrCreate(createIdentity: () -> ByteArray): ByteArray {
        if (encryptedFile.baseFile.exists()) return decrypt(readFile())

        val identity = createIdentity()
        require(identity.size == IdentityBytes) { "Invalid device identity" }
        var saved = false
        try {
            writeFile(encrypt(identity))
            saved = true
            return identity
        } finally {
            if (!saved) identity.fill(0)
        }
    }

    private fun encrypt(plaintext: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(AesGcmTransformation)
        cipher.init(Cipher.ENCRYPT_MODE, encryptionKey())
        cipher.updateAAD(AssociatedData)
        val ciphertext = cipher.doFinal(plaintext)
        return ByteArrayOutputStream(HEADER_BYTES + cipher.iv.size + ciphertext.size).use { bytes ->
            bytes.write(FormatVersion)
            bytes.write(cipher.iv.size)
            bytes.write(cipher.iv)
            bytes.write(ciphertext)
            bytes.toByteArray()
        }
    }

    private fun decrypt(encoded: ByteArray): ByteArray {
        require(encoded.size > HEADER_BYTES && encoded[0].toInt() == FormatVersion) { "Unreadable device identity" }
        val ivSize = encoded[1].toUByte().toInt()
        require(ivSize in MIN_IV_BYTES..MAX_IV_BYTES && encoded.size > HEADER_BYTES + ivSize) {
            "Unreadable device identity"
        }
        val cipher = Cipher.getInstance(AesGcmTransformation)
        cipher.init(Cipher.DECRYPT_MODE, encryptionKey(), GCMParameterSpec(GCM_TAG_BITS, encoded, HEADER_BYTES, ivSize))
        cipher.updateAAD(AssociatedData)
        return cipher.doFinal(encoded, HEADER_BYTES + ivSize, encoded.size - HEADER_BYTES - ivSize).also {
            require(it.size == IdentityBytes) { "Unreadable device identity" }
        }
    }

    private fun encryptionKey(): SecretKey {
        val keyStore = KeyStore.getInstance(AndroidKeyStore).apply { load(null) }
        return (keyStore.getEntry(keyStoreAlias, null) as? KeyStore.SecretKeyEntry)?.secretKey
            ?: KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, AndroidKeyStore)
                .apply {
                    init(
                        KeyGenParameterSpec.Builder(
                                keyStoreAlias,
                                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                            )
                            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                            .setRandomizedEncryptionRequired(true)
                            .build()
                    )
                }
                .generateKey()
    }

    private fun readFile(): ByteArray =
        encryptedFile.openRead().use { input ->
            input.readBytes().also { require(it.size <= MaximumEncryptedBytes) { "Unreadable device identity" } }
        }

    private fun writeFile(bytes: ByteArray) {
        val output = encryptedFile.startWrite()
        var committed = false
        try {
            output.write(bytes)
            output.fd.sync()
            encryptedFile.finishWrite(output)
            committed = true
        } finally {
            if (!committed) encryptedFile.failWrite(output)
        }
    }

    private companion object {
        const val HEADER_BYTES = 2
        const val MIN_IV_BYTES = 12
        const val MAX_IV_BYTES = 16
        const val GCM_TAG_BITS = 128
        const val IDENTIFIER_CHARACTERS = 32
        const val AndroidKeyStore = "AndroidKeyStore"
        const val AesGcmTransformation = "AES/GCM/NoPadding"
        const val FormatVersion = 1
        const val IdentityBytes = 32
        const val MaximumEncryptedBytes = IdentityBytes + 64
        val AssociatedData = "dev.remoteagent.mobile/iroh-identity".encodeToByteArray()

        fun stableIdentifier(reference: String): String {
            require(reference.isNotBlank()) { "Missing host identity" }
            val digest = MessageDigest.getInstance("SHA-256").digest(reference.encodeToByteArray())
            return Base64.getUrlEncoder().withoutPadding().encodeToString(digest).take(IDENTIFIER_CHARACTERS)
        }
    }
}
