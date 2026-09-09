package dev.remoteagent.mobile

import kotlinx.serialization.Serializable

@Serializable data class HostAccount(val id: String, val email: String, val planType: String = "")

@Serializable
data class HostAccountList(val accounts: List<HostAccount>, val selectedId: String? = null, val error: String? = null)

@Serializable data class HostAccountSelection(val selectedId: String, val persistenceError: String? = null)

@Serializable data class HostAccountLogin(val loginId: String, val userCode: String, val verificationUrl: String)

@Serializable data class HostAccountLoginStatus(val completed: Boolean, val accountId: String? = null)

data class AgentSettingsState(
    val accounts: List<HostAccount> = emptyList(),
    val selectedId: String? = null,
    val error: String? = null,
    val selecting: Boolean = false,
    val login: HostAccountLogin? = null,
    val startingLogin: Boolean = false,
    val models: List<CodexModel> = emptyList(),
    val options: CodexTurnOptions = CodexTurnOptions(),
    val loadingModels: Boolean = false,
    val modelError: String? = null,
    internal val modelRevision: Long = 0,
) {
    val selectedModel: String
        get() = options.model.orEmpty()

    val selectedEffort: String
        get() = options.effort.orEmpty()

    val currentModel: CodexModel?
        get() = models.firstOrNull { it.model == options.model }
}

internal data class SettingsContext(val host: String?, val generation: Long?, val connected: Boolean)
