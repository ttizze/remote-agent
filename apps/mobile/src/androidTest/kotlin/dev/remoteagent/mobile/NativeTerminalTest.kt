package dev.remoteagent.mobile

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

class NativeTerminalTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun nativeTerminalRetainsShellAfterReopening() = runBlocking {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val base = instrumentation.targetContext
        val token = UUID.randomUUID().toString()
        val isolated = object : ContextWrapper(base) {
            override fun getFilesDir() = File(base.cacheDir, "terminal-$token").apply { mkdirs() }
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                base.getSharedPreferences("$token-$name", mode)
        }
        val models = ViewModelStore()
        lateinit var model: AndroidAppModel
        suspend fun perform(intent: Intent) = withContext(Dispatchers.Main) {
            val complete = CompletableDeferred<Result<Outcome>>()
            model.perform(intent) { complete.complete(it) }
            complete.await().getOrThrow()
        }
        try {
            withContext(Dispatchers.Main) {
                model = ViewModelProvider(models, viewModelFactory { initializer { AndroidAppModel(isolated) } })[AndroidAppModel::class.java]
                model.pair(File(base.cacheDir, "fixture-invitation.json").readText())
            }
            withTimeout(30_000) {
                while (withContext(Dispatchers.Main) {
                    assertNull("Pairing must not fail while waiting for models", model.notice)
                    model.busy || !model.snapshot.connected() || model.snapshot.models().isEmpty()
                }) delay(20)
            }
            perform(Intent.NewChat(requireNotNull(InstrumentationRegistry.getArguments().getString("cwd"))))
            val visible = mutableStateOf(true)
            compose.setContent {
                MaterialTheme {
                    if (visible.value) TerminalDialog(model.snapshot, model::perform) { visible.value = false }
                }
            }
            val device = UiDevice.getInstance(instrumentation)
            fun type(command: String) {
                compose.waitUntil(10_000) { device.hasObject(By.res("dev.remoteagent.mobile", "native_terminal")) }
                device.findObject(By.res("dev.remoteagent.mobile", "native_terminal")).click()
                compose.waitUntil(10_000) { device.hasObject(By.res("dev.remoteagent.mobile", "native_terminal").focused(true)) }
                instrumentation.sendStringSync(command)
                device.pressKeyCode(android.view.KeyEvent.KEYCODE_ENTER)
            }
            compose.waitUntil(10_000) { device.hasObject(By.text("実行中")) }
            type("BEX_NATIVE=17")
            assertTrue(device.takeScreenshot(File(base.getExternalFilesDir(null), "terminal-input.png")))
            device.findObject(By.text("閉じる")).click()
            compose.waitUntil(10_000) { !visible.value }
            compose.runOnIdle { visible.value = true }
            compose.waitUntil(10_000) { device.hasObject(By.text("実行中")) }
            type("exit \$BEX_NATIVE")
            compose.waitUntil(10_000) { device.hasObject(By.text("終了 · 17")) }
            assertTrue(device.takeScreenshot(File(base.getExternalFilesDir(null), "terminal-retained.png")))
        } catch (error: Throwable) {
            val device = UiDevice.getInstance(instrumentation)
            device.takeScreenshot(File(base.getExternalFilesDir(null), "terminal-failure.png"))
            device.dumpWindowHierarchy(File(base.getExternalFilesDir(null), "terminal-failure.xml"))
            throw error
        } finally {
            withContext(Dispatchers.Main) { models.clear() }
        }
    }
}
