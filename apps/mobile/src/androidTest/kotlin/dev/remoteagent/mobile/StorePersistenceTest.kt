package dev.remoteagent.mobile

import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.remoteagent.core.AgentException
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.Intent
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class StorePersistenceTest {
    @Test
    fun nativeDraftsRoundTripAcrossIndependentHosts() = runBlocking {
        val base = InstrumentationRegistry.getInstrumentation().targetContext
        val token = UUID.randomUUID().toString()
        val directory = File(base.cacheDir, "snapshot-test-$token").apply { mkdirs() }
        val context =
            object : ContextWrapper(base) {
                override fun getFilesDir(): File = directory

                override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                    base.getSharedPreferences("$token-$name", mode)
            }
        val repository = AndroidMobileRepository(context)
        val store = AgentStore.offline(byteArrayOf())
        try {
            store.dispatch(Intent.NewChat("/fixture")).wait()
            val key = store.snapshot().navigation().draftKey
            store.dispatch(Intent.SetDraftText(key, "日本語の下書き")).wait()
            store.dispatch(Intent.AddAttachment(key, Attachment("/fixture/photo.png", "photo.png", true))).wait()
            repository.save("host-a", store.snapshot().serialize())
            store.dispatch(Intent.SetDraftText(key, "別の Host の下書き")).wait()
            repository.save("host-b", store.snapshot().serialize())
            val restoredA = AgentStore.offline(repository.load("host-a"))
            val restoredB = AgentStore.offline(repository.load("host-b"))
            try {
                assertEquals("日本語の下書き", restoredA.snapshot().draft(key).text)
                assertEquals("別の Host の下書き", restoredB.snapshot().draft(key).text)
                assertEquals("/fixture/photo.png", restoredA.snapshot().draft(key).attachments.single().path)
                assertEquals(key, restoredA.snapshot().navigation().draftKey)
                assertFalse(restoredA.snapshot().connected())
            } finally {
                restoredA.shutdown()
                restoredB.shutdown()
            }
        } finally {
            store.shutdown()
            directory.deleteRecursively()
            base.deleteSharedPreferences("$token-agent-hosts")
        }
    }

    @Test
    fun shutdownCompletesTheNativeSnapshotWait() = runBlocking {
        val store = AgentStore.offline(byteArrayOf())
        val waiter =
            async(start = CoroutineStart.UNDISPATCHED) {
                var previous = store.snapshot()
                var closed = false
                while (!closed) {
                    try {
                        previous = store.nextSnapshot(previous)
                    } catch (_: AgentException) {
                        closed = true
                    }
                }
                closed
            }
        try {
            store.shutdown()
            assertTrue(withTimeout(5_000) { waiter.await() })
        } finally {
            waiter.cancel()
            store.shutdown()
        }
    }
}
