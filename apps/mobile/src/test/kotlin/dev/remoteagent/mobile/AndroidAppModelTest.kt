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
import org.junit.Assert.assertTrue
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
    fun modelPreferencesKeepsEmptyStorageEmptyAndReportsDecodeFailures() {
        val context = RuntimeEnvironment.getApplication()
        val preferences = context.getSharedPreferences("agent-hosts", 0)
        val repository = AndroidMobileRepository(context)
        try {
            preferences.edit().putString("orchestration-model-defaults", "").commit()
            assertTrue(repository.modelPreferences().getOrThrow().isEmpty())

            preferences.edit().putString("orchestration-model-defaults", "%not-base64").commit()
            val failure = repository.modelPreferences().exceptionOrNull()
            assertNotNull(failure)
            assertEquals("Saved model preferences could not be read; defaults were restored.", failure?.message)
        } finally {
            preferences.edit().remove("orchestration-model-defaults").commit()
        }
    }

    @Test
    fun corruptModelPreferencesAreRewrittenByCoreAndKeepTheStorageNotice() = runTest {
        Dispatchers.setMain(StandardTestDispatcher(testScheduler))
        val context = RuntimeEnvironment.getApplication()
        val preferences = context.getSharedPreferences("agent-hosts", 0)
        preferences.edit().clear().putString("orchestration-model-defaults", "%not-base64").commit()
        val repository = AndroidMobileRepository(context)
        val model = AndroidAppModel(context)
        val viewModels = ViewModelStore().apply { put("corrupt-preferences", model) }
        try {
            val persisted = repository.modelPreferences().getOrThrow()
            assertTrue(persisted.isNotEmpty())
            assertTrue(model.snapshot.serializeModelPreferences().contentEquals(persisted))
            assertEquals("Saved model preferences could not be read; defaults were restored.", model.notice)
        } finally {
            viewModels.clear()
            runCurrent()
            Thread.sleep(20)
            runCurrent()
            preferences.edit().clear().commit()
            Dispatchers.resetMain()
        }
    }

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
        assertEquals(expected, pushDeviceIdForUnregister(context, "host-cold-start", null))
    }

    @Test
    fun activityOverviewDeepLinkOpensTheHostsSurface() = runTest {
        Dispatchers.setMain(StandardTestDispatcher(testScheduler))
        val context = RuntimeEnvironment.getApplication()
        val repository = AndroidMobileRepository(context)
        repository.saveProfiles(listOf(HostProfile("overview-host", "Overview", "ticket")))
        repository.selected = null
        val model = AndroidAppModel(context)
        val viewModels = ViewModelStore().apply { put("activity-overview", model) }
        try {
            model.openActivityOverviewDeepLink()
            assertEquals(Route.Hosts, model.route)
        } finally {
            viewModels.clear()
            runCurrent()
            repository.saveProfiles(emptyList())
            repository.selected = null
            Dispatchers.resetMain()
        }
    }
}
