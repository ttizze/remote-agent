package dev.remoteagent.mobile

import android.content.Context
import android.util.AtomicFile
import java.io.File

/** App-private, non-secret JSON persistence. Pairing tickets and device PKCS#8 material stay out. */
class AndroidMobileRepository(context: Context) : MobileRepository {
    private val file = AtomicFile(File(context.filesDir, "mobile-state.v1.json"))

    override fun load(): AppState = runCatching {
        val bytes = file.openRead().use { it.readBytes() }
        when (val result = MobileStateCodec.decode(bytes)) {
            is MobileStateDecodeResult.Success -> result.value
            is MobileStateDecodeResult.Failure -> AppState()
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
