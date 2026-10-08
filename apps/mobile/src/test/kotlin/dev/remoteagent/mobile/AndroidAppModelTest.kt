package dev.remoteagent.mobile

import androidx.lifecycle.ViewModelStore
import dev.remoteagent.core.Snapshot
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(manifest = Config.NONE, sdk = [35])
class AndroidAppModelTest {
    @Test
    fun damagedStateRecoversWithoutRemovingTheHost() = runTest {
        Dispatchers.setMain(StandardTestDispatcher(testScheduler))
        val context = RuntimeEnvironment.getApplication()
        val repository = AndroidMobileRepository(context)
        val saved = "invalid fixture that fails startup".encodeToByteArray()
        val preferences = "saved model preferences".encodeToByteArray()
        repository.saveProfiles(listOf(HostProfile("fixture", "Fixture", "invalid-ticket")))
        repository.selected = "fixture"
        java.io.File(repository.stateFile("fixture")).writeBytes(saved)
        repository.saveModelPreferences(preferences)
        val model = AndroidAppModel(context)
        val viewModels = ViewModelStore().apply { put("fixture", model) }
        try {
            model.persist()
            for (attempt in 0 until 200) {
                runCurrent()
                if (model.notice != null) break
                Thread.sleep(10)
            }
            assertNotNull("connection errors remain visible", model.notice)
            model.editDraft("recovered draft")
            for (attempt in 0 until 200) {
                runCurrent()
                if (model.snapshot.draft().text == "recovered draft") break
                Thread.sleep(10)
            }
            assertEquals("recovered draft", model.snapshot.draft().text)
            model.background()
            for (attempt in 0 until 200) {
                runCurrent()
                val state = java.io.File(repository.stateFile("fixture")).readBytes()
                if (runCatching { Snapshot.restore(state).draft().text }.getOrNull() == "recovered draft") break
                Thread.sleep(10)
            }
            val state = java.io.File(repository.stateFile("fixture")).readBytes()
            assertEquals("recovered draft", Snapshot.restore(state).draft().text)
            assertEquals("fixture", repository.selected)
        } finally {
            viewModels.clear()
            runCurrent()
            Thread.sleep(20)
            runCurrent()
            Dispatchers.resetMain()
        }
    }

    @Test
    fun coldStartRemovalUsesTheRetainedHostPrincipalWithoutARegistrationCache() {
        val context = RuntimeEnvironment.getApplication()
        val expected = PushRegistrationStore.deviceId(context, "host-cold-start")
        assertEquals(
            expected,
            pushDeviceIdForUnregister(context, "host-cold-start", null),
        )
    }
}
