package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.Connection
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Presentation
import dev.remoteagent.core.generateIdentity
import dev.remoteagent.core.parseInvitation
import java.io.File
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class ModelPickerConnectionTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun connectedModelButtonRefreshesAndOpensSettingsWhileOfflineButtonStaysDisabled() = runBlocking {
        AgentStore.offline(byteArrayOf(), null, "model-picker-fixture").use { store ->
            try {
                val intents = mutableListOf<Intent>()
                val perform: (Intent, (Result<Outcome>) -> Unit) -> Unit = { intent, _ -> intents.add(intent) }
                var presentation by mutableStateOf(Presentation.fromSnapshot(store.snapshot(), ""))
                compose.setContent {
                    MaterialTheme {
                        ModelPickerButton(presentation.composer(), perform) { selected, dismiss ->
                            ModelPickerDialog(presentation.settings(), selected, perform, dismiss)
                        }
                    }
                }
                compose.onNodeWithTag("model.settings").assertIsNotEnabled().performClick()
                compose.runOnIdle { assertTrue(intents.isEmpty()) }
                val base = InstrumentationRegistry.getInstrumentation().targetContext
                val invitation =
                    parseInvitation(
                        File(base.cacheDir, "fixture-invitation.json").readText(),
                        (System.currentTimeMillis() / 1000).toULong(),
                    )
                withTimeout(30_000) {
                    store.reconnect(
                        Connection(
                            invitation.endpoint,
                            generateIdentity(),
                            invitation.invitation.toString(),
                            false,
                            "Model picker fixture",
                        )
                    )
                }
                compose.runOnIdle {
                    assertTrue(store.snapshot().connected())
                    presentation = Presentation.fromSnapshot(store.snapshot(), "")
                }
                compose.onNodeWithTag("model.settings").assertIsEnabled().performClick()
                compose.onNodeWithText("モデルと実行設定").assertExists()
                compose.onNodeWithText("完了").performClick()
                compose.onNodeWithText("モデルと実行設定").assertDoesNotExist()
                compose.runOnIdle { assertEquals(listOf(Intent.RefreshModels), intents) }
            } finally {
                store.shutdown()
            }
        }
    }
}
