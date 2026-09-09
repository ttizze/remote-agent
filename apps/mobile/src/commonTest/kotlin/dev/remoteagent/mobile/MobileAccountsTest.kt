package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonElement

internal class MobileAccountsTest : MobileControllerTestFixture() {
    @Test
    fun repeated_login_actions_share_the_pending_login_and_failed_cancel_keeps_it() = runBlocking {
        val scope = persistenceScope()
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        controller.connect(profile, scope)
        val started = CompletableDeferred<Unit>()
        val response = CompletableDeferred<GatewayResult<String>>()
        var loginCalls = 0
        var state = AgentSettingsState()
        val subscription = controller.observeSettings { state = it }
        gateway.agentBlock = { command ->
            when (command) {
                AgentCommand.StartAccountLogin -> {
                    loginCalls++
                    started.complete(Unit)
                    response.await()
                }
                is AgentCommand.CancelAccountLogin -> GatewayResult.Failure("cancel failed")
                else -> null
            }
        }
        val login = scope.launch { controller.startAccountLogin() }
        withTimeout(5_000) { started.await() }
        controller.startAccountLogin()
        assertEquals(1, loginCalls)
        response.complete(
            GatewayResult.Success(
                """{"loginId":"fixture-login","userCode":"code","verificationUrl":"https://example.test/login"}"""
            )
        )
        withTimeout(5_000) { login.join() }
        controller.pauseAccountPolling()
        controller.startAccountLogin()
        assertEquals(1, loginCalls)
        controller.cancelAccountLogin()
        assertEquals("fixture-login", state.login?.loginId)
        assertEquals("cancel failed", state.error)
        assertFalse(state.startingLogin)
        subscription.cancel()
    }

    @Test
    fun retired_connection_cannot_publish_an_account_selection_or_fork_result() = runBlocking {
        val scope = persistenceScope()
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        controller.connect(profile, scope)
        var state: AgentSettingsState? = null
        val subscription = controller.observeSettings { state = it }
        val response = CompletableDeferred<GatewayResult<String>>()
        gateway.agentBlock = { response.await() }
        val selection = scope.launch { controller.selectAccount("new-account") }
        val forkResult = CompletableDeferred<GatewayResult<JsonElement>>()
        val fork = scope.launch { forkResult.complete(controller.forkThread("thread-1", "turn-1")) }
        controller.dispatch(AppAction.Disconnected(profile.id))
        response.complete(GatewayResult.Success("""{"selectedId":"new-account"}"""))
        withTimeout(5_000) {
            selection.join()
            fork.join()
        }
        assertNull(state!!.selectedId)
        assertFalse(state.selecting)
        assertNull(state.error)
        assertIs<GatewayResult.Failure>(forkResult.await())
        subscription.cancel()
    }

    @Test
    fun login_polling_pauses_and_resumes_then_selects_the_completed_account() = runBlocking {
        val scope = persistenceScope()
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        controller.connect(profile, scope)
        var statusCalls = 0
        val selected = CompletableDeferred<AgentSettingsState>()
        val subscription = controller.observeSettings { if (it.selectedId == "new-account") selected.complete(it) }
        gateway.agentBlock = { command ->
            val body =
                when (command) {
                    AgentCommand.StartAccountLogin ->
                        """{"loginId":"fixture-login","userCode":"fixture-code",
                        "verificationUrl":"https://example.test/login"}"""
                    is AgentCommand.AccountLoginStatus -> {
                        statusCalls++
                        """{"completed":true,"accountId":"new-account"}"""
                    }
                    AgentCommand.Accounts ->
                        """{"accounts":[{"id":"new-account","email":"fixture@example.test","planType":"test"}]}"""
                    is AgentCommand.SelectAccount -> """{"selectedId":"new-account"}"""
                    else -> null
                }
            body?.let { GatewayResult.Success(it) }
        }
        controller.startAccountLogin()
        controller.pauseAccountPolling()
        delay(2_100)
        assertEquals(0, statusCalls)
        controller.resumeAccountPolling()
        controller.resumeAccountPolling()
        val final = withTimeout(5_000) { selected.await() }
        assertNull(final.login)
        assertEquals(listOf("new-account"), final.accounts.map { it.id })
        assertEquals(1, statusCalls)
        subscription.cancel()
    }

    @Test
    fun changing_accounts_discards_an_older_catalog_and_resolves_choices_with_shared_policy() = runBlocking {
        val scope = persistenceScope()
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        controller.connect(profile, scope)
        val stale = CompletableDeferred<GatewayResult<String>>()
        var modelCalls = 0
        val catalog =
            """[{"id":"new-model","model":"new-model","displayName":"New model",
            "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]}]"""
        gateway.agentBlock = { command ->
            when (command) {
                AgentCommand.Models -> if (++modelCalls == 1) stale.await() else GatewayResult.Success(catalog)
                is AgentCommand.SelectAccount -> GatewayResult.Success("""{"selectedId":"new-account"}""")
                else -> null
            }
        }
        val observation = controller.observeSettings {}
        try {
            assertEquals(1, modelCalls)
            controller.selectAccount("new-account")
            assertEquals(listOf("new-model"), controller.settingsState.models.map { it.id })
            stale.complete(GatewayResult.Success("[]"))
            assertEquals(listOf("new-model"), controller.settingsState.models.map { it.id })
            controller.chooseModel("new-model")
            controller.chooseEffort("unsupported")
            assertEquals("low", controller.settingsState.selectedEffort)
            assertEquals(CodexTurnOptions("new-model", "low"), controller.state.turnChoices[profile.id])
        } finally {
            observation.cancel()
        }
    }
}
