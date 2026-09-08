package dev.remoteagent.mobile

import android.content.Context
import android.util.AtomicFile
import java.io.File

/** App-private JSON persistence for relay profiles and bounded display cache. */
class AndroidMobileRepository(context: Context) : MobileRepository {
    private val file = AtomicFile(File(context.filesDir, "mobile-state.v1.json"))

    override fun load(): AppState =
        runCatching {
                val bytes = file.openRead().use { it.readBytes() }
                when (val result = MobileStateCodec.decode(bytes)) {
                    is MobileStateDecodeResult.Success -> result.value
                    is MobileStateDecodeResult.Failure ->
                        AppState().also { if (result.reason == MobileStateDecodeReason.UnsupportedVersion) save(it) }
                }
            }
            .getOrElse { AppState() }

    override fun save(state: AppState) {
        try {
            val bytes = MobileStateCodec.encode(state)
            require(bytes.size <= MobileStateCodec.MaxInputBytes) { "Mobile cache exceeds storage limit" }
            val output = file.startWrite()
            var committed = false
            try {
                output.write(bytes)
                output.fd.sync()
                file.finishWrite(output)
                committed = true
            } finally {
                if (!committed) file.failWrite(output)
            }
        } catch (failure: java.io.IOException) {
            throw MobilePersistenceException(failure)
        }
    }
}
