package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject

internal class ConversationHistoryCorpusTest {
    @Test
    fun native_projection_uses_the_same_history_corpus_as_rust() {
        val resource = requireNotNull(javaClass.getResourceAsStream("/history.json"))
        val cases = resource.bufferedReader().use { Json.parseToJsonElement(it.readText()).jsonArray }
        for (entry in cases) {
            val case = entry.jsonObject
            val previous = codexThreadSnapshot(case.getValue("previous"))
            val incoming = codexThreadSnapshot(case.getValue("incoming"))
            val merge = {
                if (case.string("operation") == "older")
                    mergeOlderHistory(previous, incoming, case.string("turnId"), case.string("cursor"))
                else mergeHistoryRefresh(previous, incoming)
            }
            val error = case.string("errorContains")
            if (error != null) {
                assertTrue(
                    assertFailsWith<IllegalStateException> { merge() }.message!!.contains(error),
                    case.string("name"),
                )
            } else {
                assertEquals(codexThreadSnapshot(case.getValue("expected")), merge(), case.string("name"))
            }
        }
    }
}
