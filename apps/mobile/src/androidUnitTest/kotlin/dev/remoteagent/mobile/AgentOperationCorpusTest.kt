package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.decodeFromJsonElement

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
            val failure =
                case.string("errorContains")?.let { message ->
                    GatewayResult.Failure(message, case["errorRaw"]?.takeUnless { it == JsonNull })
                }
            val gateway =
                FakeHostGateway().apply {
                    agentBlock = { actual ->
                        assertEquals(commandJson, Json.parseToJsonElement(actual.encode()), name)
                        failure ?: GatewayResult.Success(case.getValue("result").toString())
                    }
                }
            val client = CommonCodexClient(gateway, deferItemDetails = true)
            val projected = project(client, command)
            if (failure != null) {
                assertEquals(failure, projected, name)
                continue
            }
            val value = assertIs<GatewayResult.Success<*>>(projected, name).value
            val expected = case.getValue("result")
            assertProjection(expected, value, command, name)
        }
    }

    private suspend fun project(client: CommonCodexClient, command: AgentCommand): GatewayResult<*> =
        when (command) {
            is AgentCommand.ListThreads -> client.listThreads(profile, command.query)
            is AgentCommand.ReadThread -> client.readThread(profile, command.threadId)
            is AgentCommand.ReadOlder ->
                client.readOlderHistory(profile, command.threadId, command.cursor, command.turnId)
            is AgentCommand.ReadItem ->
                client.readItemDetails(profile, command.threadId, command.turnId, command.itemId)
            is AgentCommand.StartThread ->
                client.startThread(profile, command.cwd, CodexTurnOptions(command.model, null))
            is AgentCommand.StartTurn ->
                client.startTurn(
                    profile,
                    command.threadId,
                    command.cwd,
                    command.input,
                    command.resume,
                    CodexTurnOptions(command.model, command.effort),
                )
            is AgentCommand.SteerTurn -> client.steerTurn(profile, command.threadId, command.turnId, command.input)
            is AgentCommand.QueueTurn -> client.queueTurn(profile, command.threadId, command.input)
            is AgentCommand.InterruptTurn -> client.interrupt(profile, command.threadId, command.turnId)
            AgentCommand.Models -> client.listModels(profile)
            else -> client.command(profile, command)
        }

    private fun assertProjection(expected: JsonElement, value: Any?, command: AgentCommand, name: String) {
        when (value) {
            is ThreadReadResult -> assertSnapshot(expected, value.thread, name)
            is ThreadSnapshot -> assertSnapshot(expected, value, name)
            is ThreadListPage -> {
                val expectedThreads = (expected as JsonObject).getValue("data") as JsonArray
                assertEquals(expectedThreads.map { (it as JsonObject).string("id") }, value.threads.map { it.id }, name)
                assertEquals(expected.boolean("hasMoreChats"), value.hasMoreChats, name)
                assertEquals(expected.boolean("hasMoreProjects"), value.hasMoreProjects, name)
            }
            is JsonElement -> assertEquals(expected, value, name)
            is List<*> ->
                assertEquals(
                    (expected as JsonArray).map { (it as JsonObject).string("displayName") },
                    value.map { (it as CodexModel).displayName },
                    name,
                )
            is String -> {
                val text =
                    if (command is AgentCommand.ReadItem)
                        ((expected as JsonObject).getValue("item") as JsonObject).string("aggregatedOutput")
                    else (expected as JsonPrimitive).content
                assertEquals(text, value, name)
            }
            Unit -> assertEquals(JsonNull, expected, name)
            else -> error("Missing projection assertion for $value")
        }
    }

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
