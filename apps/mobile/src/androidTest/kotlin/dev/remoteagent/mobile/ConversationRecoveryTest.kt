package dev.remoteagent.mobile

import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
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
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** Runs against the owned Host and emulator in scripts/android-e2e.sh. */
@RunWith(AndroidJUnit4::class)
class ConversationRecoveryTest {
    @Test
    fun coreFailureClearsAfterRetryWithoutLosingDraft() = runBlocking {
        val base = InstrumentationRegistry.getInstrumentation().targetContext
        val invitation = File(base.cacheDir, "fixture-invitation.json").readText()
        val cwd = requireNotNull(InstrumentationRegistry.getArguments().getString("cwd"))
        val token = UUID.randomUUID().toString()
        // The runner owns the entire emulator, including these files until shutdown finishes.
        val context =
            object : ContextWrapper(base) {
                override fun getFilesDir(): File = File(base.cacheDir, "model-$token").apply { mkdirs() }

                override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                    base.getSharedPreferences("$token-$name", mode)
            }
        withTimeout(30_000) {
            withContext(Dispatchers.Main) {
                val models = ViewModelStore()
                val model =
                    ViewModelProvider(models, viewModelFactory { initializer { AndroidAppModel(context) } })[
                        AndroidAppModel::class.java]
                suspend fun perform(intent: Intent): Result<Outcome> {
                    val result = CompletableDeferred<Result<Outcome>>()
                    model.perform(intent) { result.complete(it) }
                    return result.await()
                }
                try {
                    model.pair(invitation)
                    while (model.busy || !model.snapshot.connected() || model.snapshot.models().isEmpty()) delay(10)
                    perform(Intent.NewChat(cwd)).getOrThrow()
                    perform(Intent.SetDraftText(model.draftKey, "Recovery fixture input")).getOrThrow()
                    perform(Intent.Submit(null, UUID.randomUUID().toString())).getOrThrow()
                    val id = requireNotNull(model.snapshot.navigation().threadId)
                    while (true) {
                        val thread =
                            JSONObject(model.snapshot.serialize().decodeToString())
                                .getJSONObject("conversations_v2")
                                .getJSONObject(id)
                        val turns = thread.getJSONArray("turns")
                        if (
                            turns.length() > 0 &&
                                turns.getJSONObject(turns.length() - 1).getString("status") == "completed"
                        ) {
                            assertTrue(thread.toString().contains("Recovery fixture input"))
                            break
                        }
                        delay(10)
                    }
                    assertEquals("", model.snapshot.draft(id).text)
                    perform(Intent.SetDraftText(id, "preserved draft")).getOrThrow()
                    assertTrue(perform(Intent.ReadThread(ReadThread("missing-thread", open = true))).isFailure)
                    assertNotNull(model.snapshot.error())
                    perform(Intent.ReadThread(ReadThread(id, open = true))).getOrThrow()
                    assertNull(model.snapshot.error())
                    assertEquals("preserved draft", model.snapshot.draft(id).text)
                    assertNull("The recovered core error must not remain as a local notice", model.notice)
                    model.notice = "local persistence failure"
                    perform(Intent.ReadThread(ReadThread(id, open = true))).getOrThrow()
                    assertEquals("local persistence failure", model.notice)
                    // The foreground adapter calls this same method. Reconnect
                    // while the cached flag is still connected, retaining the draft.
                    model.connect()
                    while (model.busy) delay(10)
                    assertTrue(model.snapshot.connected())
                    assertEquals(id, model.snapshot.navigation().threadId)
                    assertEquals("preserved draft", model.snapshot.draft(id).text)
                    assertNull(model.notice)
                    perform(Intent.Submit(id, UUID.randomUUID().toString())).getOrThrow()
                    while (true) {
                        val thread = JSONObject(model.snapshot.serialize().decodeToString())
                            .getJSONObject("conversations_v2").getJSONObject(id)
                        val turns = thread.getJSONArray("turns")
                        if (turns.length() == 2 && turns.getJSONObject(1).getString("status") == "completed") {
                            assertTrue(turns.getJSONObject(1).toString().contains("preserved draft"))
                            break
                        }
                        delay(10)
                    }
                    assertEquals("", model.snapshot.draft(id).text)
                    assertNull(model.snapshot.error())
                    assertNull(model.notice)
                } finally {
                    models.clear()
                }
            }
        }
    }
}
