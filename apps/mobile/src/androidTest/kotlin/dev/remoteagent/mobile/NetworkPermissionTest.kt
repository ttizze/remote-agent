package dev.remoteagent.mobile

import android.Manifest
import android.content.pm.PackageManager
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import java.io.File
import java.io.IOException
import java.net.InetSocketAddress
import java.net.Socket
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** Run on a fresh API 37 emulator with local-network permission initially denied. */
@RunWith(AndroidJUnit4::class)
class NetworkPermissionTest {
    @Test
    fun deniedPermissionCanBeRetriedAndGrantedPermissionSurvivesRecreation() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        assertEquals(
            PackageManager.PERMISSION_DENIED,
            context.checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK),
        )
        val device = UiDevice.getInstance(instrumentation)
        ActivityScenario.launch(MainActivity::class.java).use { activity ->
            val permissionVisible = device.wait(Until.hasObject(By.text("許可して接続")), 10_000)
            assertTrue(device.takeScreenshot(File(context.getExternalFilesDir(null), "network-permission.png")))
            assertTrue(permissionVisible)
            device.findObject(By.text("許可して接続")).click()
            val deny = By.res("com.android.permissioncontroller", "permission_deny_button")
            assertTrue(device.wait(Until.hasObject(deny), 10_000))
            device.findObject(deny).click()
            assertTrue(device.wait(Until.hasObject(By.text("アプリの設定を開く")), 10_000))
            assertFalse(device.hasObject(By.text("PCとペアリング")))
            assertThrows(IOException::class.java) { readLocalHost() }
            device.findObject(By.text("アプリの設定を開く")).click()
            assertTrue(device.wait(Until.hasObject(By.pkg("com.android.settings")), 10_000))
            device.pressBack()
            assertTrue(device.wait(Until.hasObject(By.text("インターネット経由で接続")), 10_000))
            device.findObject(By.text("インターネット経由で接続")).click()
            assertTrue(device.wait(Until.hasObject(By.text("PCとペアリング")), 10_000))
            assertThrows(IOException::class.java) { readLocalHost() }
            activity.recreate()
            assertTrue(device.wait(Until.hasObject(By.text("PCとペアリング")), 10_000))
        }
        ActivityScenario.launch(MainActivity::class.java).use { activity ->
            assertTrue(device.wait(Until.hasObject(By.text("許可して接続")), 10_000))
            device.findObject(By.text("許可して接続")).click()
            val allow = By.res("com.android.permissioncontroller", "permission_allow_button")
            assertTrue(device.wait(Until.hasObject(allow), 10_000))
            device.findObject(allow).click()
            assertTrue(device.wait(Until.hasObject(By.text("PCとペアリング")), 10_000))
            assertEquals(
                PackageManager.PERMISSION_GRANTED,
                context.checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK),
            )
            assertEquals("bex-os-network-ok", readLocalHost())
            activity.recreate()
            assertTrue(device.wait(Until.hasObject(By.text("PCとペアリング")), 10_000))
            assertFalse(device.hasObject(By.text("許可して接続")))
            assertEquals("bex-os-network-ok", readLocalHost())
            assertTrue(device.takeScreenshot(File(context.getExternalFilesDir(null), "network-permission-granted.png")))
        }
    }

    private fun readLocalHost(): String? {
        val port = requireNotNull(InstrumentationRegistry.getArguments().getString("networkPort")).toInt()
        return Socket().use { socket ->
            socket.soTimeout = 2_000
            socket.connect(InetSocketAddress("10.0.2.2", port), 2_000)
            socket.getInputStream().bufferedReader().readLine()
        }
    }
}
