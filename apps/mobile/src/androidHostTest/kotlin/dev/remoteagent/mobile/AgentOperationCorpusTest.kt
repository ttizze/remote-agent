package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonObject

internal class AgentOperationCorpusTest : MobileControllerTestFixture() {
    @Test
    fun native_intents_and_mobile_projections_use_the_rust_operation_corpus() = runSuspend {
        val cases =
            listOf("/operations.json", "/host-operations.json").flatMap { path ->
                requireNotNull(javaClass.getResourceAsStream(path)).bufferedReader().use {
                    Json.parseToJsonElement(it.readText()) as JsonArray
                }
            }
        for (entry in cases) {
            val case = entry as JsonObject
            val name = case.string("name")!!
            val commandJson = case.getValue("command")
            val command = Json.decodeFromJsonElement<AgentCommand>(commandJson)
            assertEquals(commandJson, Json.parseToJsonElement(command.encode()), name)
            val expected = case.fixtureValue("result")
            if (case.string("errorContains") != null) continue
            when (command) {
                is AgentCommand.ReadThread,
                is AgentCommand.ReadOlder,
                is AgentCommand.StartThread -> assertSnapshot(expected, codexThreadFromResponse(expected), name)
                is AgentCommand.ListThreads -> {
                    val page = parseThreadListPage(expected)
                    val raw = expected as JsonObject
                    assertEquals(
                        raw.array("data")!!.map { it.jsonObject.string("id") },
                        page.threads.map { it.id },
                        name,
                    )
                    assertEquals(raw.boolean("hasMoreChats"), page.hasMoreChats, name)
                    assertEquals(raw.boolean("hasMoreProjects"), page.hasMoreProjects, name)
                }
                is AgentCommand.ReadItem -> {
                    val item = (expected as JsonObject).getValue("item")
                    assertEquals(
                        item.jsonObject.string("aggregatedOutput"),
                        codexItem(item).expandedThreadItemBody(),
                        name,
                    )
                }
                AgentCommand.Models -> {
                    val gateway =
                        FakeHostGateway().apply { agentBlock = { GatewayResult.Success(expected.toString()) } }
                    val models =
                        assertIs<GatewayResult.Success<List<CodexModel>>>(gateway.listModels(profile), name).value
                    assertEquals(
                        (expected as JsonArray).map { it.jsonObject.string("displayName") },
                        models.map { it.displayName },
                        name,
                    )
                }
                else -> Unit
            }
        }
    }

    private fun JsonObject.fixtureValue(field: String): JsonElement =
        string("${field}Ref")?.removePrefix("/")?.split("/")?.fold(this as JsonElement) { value, key ->
            if (value is JsonArray) value[key.toInt()] else value.jsonObject.getValue(key)
        } ?: get(field) ?: JsonNull

    private fun assertSnapshot(expected: JsonElement, snapshot: ThreadSnapshot, name: String) {
        val thread = (expected as JsonObject).getValue("thread") as JsonObject
        assertEquals<JsonObject?>(thread.without("turns"), snapshot.raw, name)
        val turns = thread.getValue("turns") as JsonArray
        assertEquals(turns.map { (it as JsonObject).string("id") }, snapshot.turns.map { it.id }, name)
        turns.zip(snapshot.turns).forEach { (raw, turn) ->
            assertEquals<JsonObject?>((raw as JsonObject).without("items"), turn.raw, name)
            assertEquals(
                (raw.getValue("items") as JsonArray).map { (it as JsonObject).string("id") },
                turn.items.map { it.id },
                name,
            )
        }
    }
}
