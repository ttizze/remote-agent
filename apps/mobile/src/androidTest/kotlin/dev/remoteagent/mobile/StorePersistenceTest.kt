package dev.remoteagent.mobile

import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.remoteagent.core.AgentException
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.ConversationRowContent
import dev.remoteagent.core.DraftKey
import dev.remoteagent.core.Intent
import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.Snapshot
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class StorePersistenceTest {
    @Test
    fun projectionRetainsBothAnswersAndPublishedValuesAcrossDraftEdits() = runBlocking {
        val session = SessionRef("thread")
        val key = DraftKey.Session(session)
        val identity = JSONObject("""{"id":"thread"}""")
        val persisted =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put(
                    "navigation",
                    JSONObject()
                        .put("thread_id", identity)
                        .put("draft_key", JSONObject().put("Session", JSONObject().put("session", identity)))
                        .put("cwd", "/fixture"),
                )
                .put(
                    "conversations",
                    JSONArray()
                        .put(
                            JSONArray()
                                .put(identity)
                                .put(
                                    JSONObject(
                                        """{"provider":"codex","id":{"id":"thread"},"turns":[
                {"id":"repeated","items":[${messageItem("first", "first answer")}]},
                {"id":"repeated","items":[${messageItem("second", "second answer")}]}
            ]}"""
                                    )
                                )
                        ),
                )
        // History is an explicit projection input, not device storage.
        val fixture = Snapshot.restore(persisted.toString().encodeToByteArray())
        val source = fixture.conversation(session)
        val bytes = fixture.serializeLocalState()
        fixture.close()
        val store = AgentStore.offline(bytes, null)
        try {
            val snapshot = store.snapshot()
            val first = requireNotNull(projectConversationRows(snapshot, source, null))
            fun answers(projection: ConversationProjection) =
                projection.rows.mapNotNull {
                    (it.content as? ConversationRowContent.Response)?.item?.presentation()?.body
                }
            assertEquals(listOf("first answer", "second answer"), answers(first))
            val edit = store.dispatch(Intent.SetDraftText(key, "new draft"))
            val latest = store.snapshot()
            assertEquals("new draft", latest.draft(key).text)
            assertEquals("", snapshot.draft(key).text)
            edit.wait()
            val second = projectConversationRows(latest, source, first)
            assertSame(first, second)
            assertEquals(listOf("first answer", "second answer"), answers(first))
            assertNull(projectConversationRows(latest, null, first))
            val reopened = requireNotNull(projectConversationRows(latest, source, null))
            assertEquals(answers(first), answers(reopened))
        } finally {
            store.shutdown()
        }
    }

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
        val store = AgentStore.offline(byteArrayOf(), null)
        try {
            store.dispatch(Intent.NewChat("/fixture")).wait()
            val key = store.snapshot().navigation().draftKey
            store.dispatch(Intent.SetDraftText(key, "日本語の下書き")).wait()
            store.dispatch(Intent.AddAttachment(key, Attachment("/fixture/photo.png", "photo.png", true))).wait()
            repository.save(
                "host-a",
                store.snapshot().serializeLocalState(),
                store.snapshot().serializeClientPreferences(),
            )
            store.dispatch(Intent.SetDraftText(key, "別の Host の下書き")).wait()
            repository.save(
                "host-b",
                store.snapshot().serializeLocalState(),
                store.snapshot().serializeClientPreferences(),
            )
            val restoredA = AgentStore.offline(repository.load("host-a"), null)
            val restoredB = AgentStore.offline(repository.load("host-b"), null)
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
        val store = AgentStore.offline(byteArrayOf(), null)
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
