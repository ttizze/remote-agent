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
    fun retired_connection_cannot_publish_an_account_selection_or_fork_result() = runBlocking {
        val scope = persistenceScope()
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        controller.connect(profile, scope)
        var state: AccountSettingsState? = null
        val subscription = controller.observeAccounts { state = it }
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
        val selected = CompletableDeferred<AccountSettingsState>()
        val subscription = controller.observeAccounts { if (it.selectedId == "new-account") selected.complete(it) }
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
                    else -> error("Unexpected account operation: $command")
                }
            GatewayResult.Success(body)
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
}
