package dev.remoteagent.mobile

import kotlinx.cinterop.ByteVar
import kotlinx.cinterop.COpaquePointer
import kotlinx.cinterop.COpaquePointerVar
import kotlinx.cinterop.CPointed
import kotlinx.cinterop.CPointer
import kotlinx.cinterop.CValuesRef
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.alloc
import kotlinx.cinterop.memScoped
import kotlinx.cinterop.ptr
import kotlinx.cinterop.reinterpret
import kotlinx.cinterop.toCValues
import kotlinx.cinterop.toKString
import kotlinx.cinterop.usePinned
import kotlinx.cinterop.value
import kotlinx.serialization.json.Json
import mobile_client.mobile_client_string_free
import platform.CoreFoundation.CFDataCreate
import platform.CoreFoundation.CFDataGetBytePtr
import platform.CoreFoundation.CFDataGetLength
import platform.CoreFoundation.CFDictionaryCreate
import platform.CoreFoundation.CFDictionaryRef
import platform.CoreFoundation.CFRelease
import platform.CoreFoundation.CFTypeRefVar
import platform.CoreFoundation.kCFBooleanTrue
import platform.Foundation.NSData
import platform.Foundation.NSFileManager
import platform.Foundation.NSHomeDirectory
import platform.Foundation.dataWithBytes
import platform.Foundation.dataWithContentsOfFile
import platform.Foundation.writeToFile
import platform.Security.SecItemAdd
import platform.Security.SecItemCopyMatching
import platform.Security.SecItemUpdate
import platform.Security.errSecItemNotFound
import platform.Security.errSecSuccess
import platform.Security.kSecAttrAccessible
import platform.Security.kSecAttrAccessibleWhenUnlockedThisDeviceOnly
import platform.Security.kSecAttrAccount
import platform.Security.kSecAttrService
import platform.Security.kSecClass
import platform.Security.kSecClassGenericPassword
import platform.Security.kSecReturnData
import platform.Security.kSecValueData

private const val DEVICE_KEY_SERVICE = "app.bex.mobile.credentials.v4"

internal const val DEFAULT_REQUEST_TIMEOUT_MS = 30_000L

internal val iosJson = Json {
    ignoreUnknownKeys = true
    isLenient = false
    classDiscriminator = "type"
}

/** Keychain owns opaque Rust PKCS#8 bytes, never a Swift CryptoKit key. */
@OptIn(ExperimentalForeignApi::class)
internal object IosCredentialStore {
    fun loadOrGenerate(reference: String, generate: () -> ByteArray): ByteArray =
        load(reference) ?: generate().also { save(reference, it) }

    fun load(reference: String): ByteArray? = memScoped {
        val result = alloc<CFTypeRefVar>()
        result.value = null
        withQuery(reference, true, null) { query ->
            when (val status = SecItemCopyMatching(query, result.ptr)) {
                errSecSuccess -> {
                    val data = requireNotNull(result.value)
                    try {
                        data.reinterpret<CPointed>().toByteArray()
                    } finally {
                        CFRelease(data)
                    }
                }
                errSecItemNotFound -> null
                else -> error("Keychain read failed ($status)")
            }
        }
    }

    fun save(reference: String, key: ByteArray) {
        withQuery(reference, false, null) { query ->
            val data = key.toCFData()
            try {
                val attributes =
                    CFDictionaryCreate(
                        null,
                        listOf<COpaquePointer?>(requireNotNull(kSecValueData).reinterpret<CPointed>())
                            .toOpaqueCValues(),
                        listOf<COpaquePointer?>(data.reinterpret<CPointed>()).toOpaqueCValues(),
                        1,
                        null,
                        null,
                    ) ?: error("Keychain update could not be created")
                val status =
                    try {
                        SecItemUpdate(query, attributes)
                    } finally {
                        CFRelease(attributes)
                    }
                if (status == errSecItemNotFound) {
                    withQuery(reference, false, key) {
                        check(SecItemAdd(it, null) == errSecSuccess) { "Keychain write failed" }
                    }
                } else check(status == errSecSuccess) { "Keychain update failed ($status)" }
            } finally {
                CFRelease(data)
            }
        }
    }

    private fun <T> withQuery(
        reference: String,
        returnsData: Boolean,
        key: ByteArray?,
        block: (CFDictionaryRef) -> T,
    ): T = memScoped {
        val service = cfString(DEVICE_KEY_SERVICE)
        val account = cfString(reference)
        val keyData = key?.toCFData()
        val keys =
            mutableListOf<COpaquePointer?>(
                requireNotNull(kSecClass).reinterpret<CPointed>(),
                requireNotNull(kSecAttrService).reinterpret<CPointed>(),
                requireNotNull(kSecAttrAccount).reinterpret<CPointed>(),
            )
        val values =
            mutableListOf<COpaquePointer?>(
                requireNotNull(kSecClassGenericPassword).reinterpret<CPointed>(),
                service.reinterpret<CPointed>(),
                account.reinterpret<CPointed>(),
            )
        if (returnsData) {
            keys += requireNotNull(kSecReturnData).reinterpret<CPointed>()
            values += requireNotNull(kCFBooleanTrue).reinterpret<CPointed>()
        }
        if (keyData != null) {
            keys += requireNotNull(kSecValueData).reinterpret<CPointed>()
            values += keyData.reinterpret<CPointed>()
            keys += requireNotNull(kSecAttrAccessible).reinterpret<CPointed>()
            values += requireNotNull(kSecAttrAccessibleWhenUnlockedThisDeviceOnly).reinterpret<CPointed>()
        }
        val dictionary =
            CFDictionaryCreate(null, keys.toOpaqueCValues(), values.toOpaqueCValues(), keys.size.toLong(), null, null)
                ?: error("Keychain query could not be created")
        try {
            block(dictionary)
        } finally {
            CFRelease(dictionary)
            keyData?.let { CFRelease(it) }
            CFRelease(account)
            CFRelease(service)
        }
    }
}

@Suppress("UNCHECKED_CAST")
@OptIn(ExperimentalForeignApi::class)
private fun List<COpaquePointer?>.toOpaqueCValues(): CValuesRef<COpaquePointerVar> =
    map { it?.reinterpret<CPointed>() }.toTypedArray().toCValues() as CValuesRef<COpaquePointerVar>

@OptIn(ExperimentalForeignApi::class)
private fun cfString(value: String) =
    platform.CoreFoundation.CFStringCreateWithCString(null, value, platform.CoreFoundation.kCFStringEncodingUTF8)
        ?: error("CFString could not be created")

@OptIn(ExperimentalForeignApi::class)
private fun ByteArray.toCFData() = usePinned {
    CFDataCreate(null, it.addressOf(0).reinterpret(), size.toLong()) ?: error("CFData could not be created")
}

@OptIn(ExperimentalForeignApi::class)
private fun CPointer<CPointed>.toByteArray(): ByteArray {
    val data = reinterpret<cnames.structs.__CFData>()
    val length = CFDataGetLength(data).toInt()
    return ByteArray(length).also { bytes ->
        bytes.usePinned { pinned ->
            CFDataGetBytePtr(data)?.let { platform.posix.memcpy(pinned.addressOf(0), it, length.toULong()) }
        }
    }
}

@OptIn(ExperimentalForeignApi::class)
private fun ByteArray.toNSData(): NSData = usePinned { NSData.Companion.dataWithBytes(it.addressOf(0), size.toULong()) }

@OptIn(ExperimentalForeignApi::class)
private fun NSData.toByteArray(): ByteArray =
    ByteArray(length.toInt()).also { bytes ->
        bytes.usePinned { pinned -> platform.posix.memcpy(pinned.addressOf(0), this.bytes, length) }
    }

/** Durable profile/cache repository. File writes replace whole JSON atomically. */
@OptIn(ExperimentalForeignApi::class)
internal class IosMobileRepository : MobileRepository {
    private val path = "${NSHomeDirectory()}/Library/Application Support/Bex/mobile-state.json"

    override fun load(): AppState {
        val state = loadState()
        val defaults = platform.Foundation.NSUserDefaults.standardUserDefaults
        val models = defaults.dictionaryForKey("bex.models.v1").orEmpty()
        val efforts = defaults.dictionaryForKey("bex.efforts.v1").orEmpty()
        if (models.isEmpty() && efforts.isEmpty()) return state
        val choices = buildMap {
            for (key in models.keys + efforts.keys) {
                if (key is String)
                    put(
                        key,
                        CodexTurnOptions(
                            (models[key] as? String)?.takeIf(String::isNotEmpty),
                            (efforts[key] as? String)?.takeIf(String::isNotEmpty),
                        ),
                    )
            }
        }
        return state.copy(turnChoices = choices + state.turnChoices)
    }

    override fun save(state: AppState) {
        NSFileManager.defaultManager.createDirectoryAtPath(
            "${NSHomeDirectory()}/Library/Application Support/Bex",
            true,
            null,
            null,
        )
        val bytes = MobileStateCodec.encode(state)
        check(bytes.toNSData().writeToFile(path, atomically = true))
        platform.Foundation.NSUserDefaults.standardUserDefaults.removeObjectForKey("bex.models.v1")
        platform.Foundation.NSUserDefaults.standardUserDefaults.removeObjectForKey("bex.efforts.v1")
    }

    private fun loadState(): AppState =
        try {
            val bytes = NSData.Companion.dataWithContentsOfFile(path)?.toByteArray() ?: return AppState()
            when (val result = MobileStateCodec.decode(bytes)) {
                is MobileStateDecodeResult.Success -> result.value
                is MobileStateDecodeResult.Failure ->
                    AppState().also { empty ->
                        // The obsolete v1 file contained relay secrets. Replace it on migration.
                        if (result.reason == MobileStateDecodeReason.UnsupportedVersion) {
                            check(MobileStateCodec.encode(empty).toNSData().writeToFile(path, atomically = true))
                        }
                    }
            }
        } catch (_: Throwable) {
            AppState()
        }
}

@OptIn(ExperimentalForeignApi::class)
internal fun takeString(value: CPointer<ByteVar>): String =
    try {
        value.toKString()
    } finally {
        mobile_client_string_free(value)
    }

@OptIn(ExperimentalForeignApi::class)
internal fun takeError(value: CPointer<ByteVar>?): String = value?.let(::takeString) ?: "mobile-client call failed"
