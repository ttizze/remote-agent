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
import dev.remoteagent.core.ReadThread
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

class VisualizationTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun realHostVisualizationSelectsAndReopens() = runBlocking {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val base = instrumentation.targetContext
        val token = UUID.randomUUID().toString()
        val isolated = object : ContextWrapper(base) {
            override fun getFilesDir() = File(base.cacheDir, "visualize-$token").apply { mkdirs() }
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
            val draft = withContext(Dispatchers.Main) { model.draftKey }
            perform(Intent.SetDraftText(draft, "[success] [visualize] Compare twelve icons"))
            perform(Intent.Submit(null, token))
            val id = withContext(Dispatchers.Main) { requireNotNull(model.snapshot.navigation().threadId) }
            withTimeout(30_000) {
                while (true) {
                    val complete = withContext(Dispatchers.Main) {
                        val thread = JSONObject(model.snapshot.serialize().decodeToString()).getJSONObject("conversations").getJSONObject(id)
                        val turns = thread.optJSONArray("turns")
                        turns != null && turns.length() > 0 && turns.getJSONObject(turns.length() - 1).optString("status") == "completed"
                    }
                    if (complete) break
                    delay(20)
                }
            }
            val visible = mutableStateOf(true)
            compose.setContent {
                MaterialTheme {
                    if (visible.value) ThreadDetailScreen(model.snapshot, model.conversation, model::perform, null, composer = {})
                }
            }
            val device = UiDevice.getInstance(instrumentation)
            fun verify(name: String) {
                // UiAutomator does not advance Compose's test frame clock.
                compose.waitUntil(20_000) { device.hasObject(By.text("01 · Git の合流")) }
                compose.waitUntil(20_000) { device.hasObject(By.text("02 ブランチ＋チェックをプレビュー")) }
                val second = device.findObject(By.text("02 ブランチ＋チェックをプレビュー"))
                assertNotNull("The second option must be accessible in the native WebView", second)
                second.click()
                compose.waitUntil(5_000) { device.hasObject(By.text("02 · ブランチ＋チェック")) }
                assertTrue(device.takeScreenshot(File(base.getExternalFilesDir(null), name)))
            }
            verify("visualize-selected.png")
            compose.runOnIdle { visible.value = false }
            compose.waitForIdle()
            perform(Intent.ShowThreadList)
            perform(Intent.ReadThread(ReadThread(id, open = true)))
            compose.runOnIdle { visible.value = true }
            verify("visualize-reopened.png")
            withContext(Dispatchers.Main) {
                assertEquals("", model.snapshot.draft(id).text)
                assertNull(model.snapshot.error())
                assertNull(model.notice)
            }
        } catch (error: Throwable) {
            val device = UiDevice.getInstance(instrumentation)
            device.takeScreenshot(File(base.getExternalFilesDir(null), "visualize-failure.png"))
            device.dumpWindowHierarchy(File(base.getExternalFilesDir(null), "visualize-failure.xml"))
            throw error
        } finally {
            withContext(Dispatchers.Main) { models.clear() }
        }
    }
}
