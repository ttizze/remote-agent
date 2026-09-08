package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject

internal class ConversationEventCorpusTest {
    @Test
    fun native_projection_uses_the_same_event_corpus_as_rust() {
        val resource = requireNotNull(javaClass.getResourceAsStream("/events.json"))
        val cases = resource.bufferedReader().use { Json.parseToJsonElement(it.readText()).jsonArray }
        for (entry in cases) {
            val case = entry.jsonObject
            var snapshot = codexThreadSnapshot(case.getValue("previous"))
            for (event in case.getValue("events").jsonArray) {
                val message = event.jsonObject
                snapshot =
                    snapshot.applyConversationEvent(
                        codexThreadEvent(message.string("method")!!, message.getValue("params"))
                    )
            }
            assertEquals(codexThreadSnapshot(case.getValue("expected")), snapshot, case.string("name"))
        }
    }

    @Test
    fun native_projection_uses_the_same_submission_corpus_as_rust() {
        val resource = requireNotNull(javaClass.getResourceAsStream("/submission.json"))
        val cases = resource.bufferedReader().use { Json.parseToJsonElement(it.readText()).jsonArray }
        val format = Json { classDiscriminator = "action" }
        for (entry in cases) {
            val case = entry.jsonObject
            val snapshot = case["snapshot"]?.takeUnless { it == JsonNull }?.let(::codexThreadSnapshot)
            val listed = case["listed"]?.takeUnless { it == JsonNull }?.let(::codexThreadSummary)
            val plan = planTurnSubmission(snapshot, listed)
            assertEquals(
                case.getValue("expected"),
                format.encodeToJsonElement(SendPlan.serializer(), plan),
                case.string("name"),
            )
        }
    }
}
