package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlinx.serialization.json.Json

/** Wire behavior moved to the shared Rust operation corpus. These tests protect the mobile adapter. */
internal class CommonCodexClientTest : MobileControllerTestFixture() {
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
        val client = CommonCodexClient(gateway)
        val options = CodexTurnOptions("model-b", "high")
        client.startThread(profile, "/workspace", options)
        client.startTurn(
            profile,
            "thread-1",
            "/workspace",
            CodexTurnInput("first", clientUserMessageId = "client-1"),
            false,
            options,
        )
        client.startTurn(
            profile.copy(hostIdentity = "other-host"),
            "thread-2",
            "/other",
            CodexTurnInput("next", clientUserMessageId = "client-2"),
            true,
        )
        assertEquals("model-b", assertIs<AgentCommand.StartThread>(commands[0]).model)
        assertEquals("high", assertIs<AgentCommand.StartTurn>(commands[1]).effort)
        assertEquals(null, assertIs<AgentCommand.StartTurn>(commands[2]).model)
        assertEquals(null, assertIs<AgentCommand.StartTurn>(commands[2]).effort)
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
