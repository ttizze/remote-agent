package dev.remoteagent.mobile

import android.content.Context
import android.util.AtomicFile
import java.io.File

/** App-private JSON persistence for relay profiles and bounded display cache. */
class AndroidMobileRepository(context: Context) : MobileRepository {
    private val file = AtomicFile(File(context.filesDir, "mobile-state.v1.json"))

    override fun load(): AppState = runCatching {
        val bytes = file.openRead().use { it.readBytes() }
        when (val result = MobileStateCodec.decode(bytes)) {
            is MobileStateDecodeResult.Success -> result.value
            is MobileStateDecodeResult.Failure -> AppState().also {
                if (result.reason == MobileStateDecodeReason.UnsupportedVersion) save(it)
            }
        }
    }.getOrElse { AppState() }

    override fun save(state: AppState) {
        val bytes = MobileStateCodec.encode(state)
        require(bytes.size <= MobileStateCodec.MaxInputBytes) { "Mobile cache exceeds storage limit" }
        val output = file.startWrite()
        try {
            output.write(bytes)
            output.fd.sync()
            file.finishWrite(output)
        } catch (error: Throwable) {
            file.failWrite(output)
            throw error
        }
    }
}
