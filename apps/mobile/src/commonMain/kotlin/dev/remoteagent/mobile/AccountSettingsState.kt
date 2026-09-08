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

internal expect fun nativeAccountTransition(event: Int, flags: Int): Int

// Stable primitive codes in mobile_client.h; these describe native facts only.
internal enum class AccountEvent {
    Select,
    StartLogin,
    ListReply,
    SelectionReply,
    LoginReply,
    StatusReply,
    CancelReply,
}

internal enum class AccountAction {
    Ignore,
    BeginSelection,
    BeginLogin,
    ApplyList,
    ApplySelection,
    ApplyLogin,
    CompleteLogin,
    ClearLogin,
    ShowError,
    SelectionFailed,
    LoginFailed,
    ContinuePolling,
}

internal fun accountAction(
    state: AccountSettingsState,
    event: AccountEvent,
    sameSelection: Boolean = false,
    failed: Boolean = false,
    completed: Boolean = false,
): AccountAction {
    val flags =
        (if (state.selecting) SELECTING else 0) or
            (if (sameSelection) SAME_SELECTION else 0) or
            (if (state.login != null) LOGIN_PRESENT else 0) or
            (if (state.startingLogin) STARTING_LOGIN else 0) or
            (if (failed) FAILED else 0) or
            (if (completed) COMPLETED else 0)
    return AccountAction.entries[nativeAccountTransition(event.ordinal, flags)]
}

private const val SELECTING = 1
private const val SAME_SELECTION = 2
private const val LOGIN_PRESENT = 4
private const val STARTING_LOGIN = 8
private const val FAILED = 16
private const val COMPLETED = 32
