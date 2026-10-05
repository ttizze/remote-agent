package dev.remoteagent.mobile

import androidx.lifecycle.ViewModelStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.Assert.assertArrayEquals
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
    fun failedStartupDoesNotOverwriteSavedStateOrSharedModelPreferences() = runTest {
        Dispatchers.setMain(StandardTestDispatcher(testScheduler))
        val context = RuntimeEnvironment.getApplication()
        val repository = AndroidMobileRepository(context)
        val saved = "invalid fixture that fails startup".encodeToByteArray()
        val preferences = "saved model preferences".encodeToByteArray()
        repository.saveProfiles(listOf(HostProfile("fixture", "Fixture", "invalid-ticket")))
        repository.selected = "fixture"
        repository.save("fixture", saved)
        repository.saveModelPreferences(preferences)
        val model = AndroidAppModel(context)
        val viewModels = ViewModelStore().apply { put("fixture", model) }
        try {
            // Cold-start/background persistence while no Store exists.
            model.persist()
            for (attempt in 0 until 200) {
                runCurrent()
                if (model.notice != null) break
                Thread.sleep(10)
            }
            assertNotNull("the fixture must fail startup", model.notice)
            model.persist()
            advanceTimeBy(300)
            runCurrent()
            Thread.sleep(100) // Allow the real I/O writer to consume any wrongly queued save.
            assertArrayEquals(saved, repository.load("fixture"))
            assertArrayEquals(preferences, repository.modelPreferences())
        } finally {
            viewModels.clear()
            runCurrent()
            Thread.sleep(20)
            runCurrent()
            Dispatchers.resetMain()
        }
    }
}
