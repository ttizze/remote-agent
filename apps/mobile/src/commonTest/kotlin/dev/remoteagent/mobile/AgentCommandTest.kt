package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull

/** Wire behavior moved to the shared Rust operation corpus. These tests protect the mobile adapter. */
internal class AgentCommandTest : MobileControllerTestFixture() {
    @Test
    fun turn_options_are_explicit_per_submission_and_not_retained_by_the_adapter() = runSuspend {
        val commands = mutableListOf<AgentCommand>()
        val gateway =
            FakeHostGateway().apply {
                agentBlock = { command ->
                    commands += command
                    GatewayResult.Success(
                        if (command is AgentCommand.StartThread)
                            """{"thread":{"id":"thread-1","cwd":"/workspace","turns":[]}}"""
                        else "\"turn-1\""
                    )
                }
            }
        val options = CodexTurnOptions("model-b", "high")
        gateway.command(profile, AgentCommand.StartThread("/workspace", options.model))
        gateway.command(
            profile,
            AgentCommand.SendTurn(
                "thread-1",
                JsonNull,
                JsonNull,
                CodexTurnInput("first", clientUserMessageId = "client-1"),
                options.model,
                options.effort,
            ),
        )
        gateway.command(
            profile.copy(hostIdentity = "other-host"),
            AgentCommand.SendTurn(
                "thread-2",
                JsonNull,
                JsonNull,
                CodexTurnInput("next", clientUserMessageId = "client-2"),
                null,
                null,
            ),
        )
        assertEquals("model-b", assertIs<AgentCommand.StartThread>(commands[0]).model)
        assertEquals("high", assertIs<AgentCommand.SendTurn>(commands[1]).effort)
        assertEquals(null, assertIs<AgentCommand.SendTurn>(commands[2]).model)
        assertEquals(null, assertIs<AgentCommand.SendTurn>(commands[2]).effort)
    }

    @Test
    fun native_agent_failures_preserve_original_payload_and_transport_failures() {
        val raw = Json.parseToJsonElement("""{"thread":{"cwd":"/workspace"},"futureField":17}""")
        val envelope = Json.parseToJsonElement("""{"message":"Invalid host/thread/read response","rawError":$raw}""")
        assertEquals(
            GatewayResult.Failure("Invalid host/thread/read response", raw),
            GatewayResult.Failure("native", envelope).agentResult(),
        )
        assertEquals(GatewayResult.Failure("offline"), GatewayResult.Failure("offline").agentResult())
    }
}
