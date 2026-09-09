package dev.remoteagent.mobile

import kotlinx.serialization.Serializable

@Serializable data class HostAccount(val id: String, val email: String, val planType: String = "")

@Serializable
data class HostAccountList(val accounts: List<HostAccount>, val selectedId: String? = null, val error: String? = null)

@Serializable data class HostAccountSelection(val selectedId: String, val persistenceError: String? = null)

@Serializable data class HostAccountLogin(val loginId: String, val userCode: String, val verificationUrl: String)

@Serializable data class HostAccountLoginStatus(val completed: Boolean, val accountId: String? = null)

data class AccountSettingsState(
    val accounts: List<HostAccount> = emptyList(),
    val selectedId: String? = null,
    val error: String? = null,
    val selecting: Boolean = false,
    val login: HostAccountLogin? = null,
    val startingLogin: Boolean = false,
)
