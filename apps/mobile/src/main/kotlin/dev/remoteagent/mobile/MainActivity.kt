package dev.remoteagent.mobile

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory

class MainActivity : ComponentActivity() {
    private val model: AndroidAppModel by viewModels {
        viewModelFactory { initializer { AndroidAppModel(applicationContext) } }
    }

    private var localNetworkGranted by mutableStateOf(false)
    private var continueOverInternet by mutableStateOf(false)
    private val requestNetwork =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            localNetworkGranted = granted
        }
    private val requestNotifications =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) {
            model.refreshPushRegistration()
        }

    override fun onResume() {
        super.onResume()
        localNetworkGranted =
            checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK) == PackageManager.PERMISSION_GRANTED
        LocalNotifications.permissionResult(
            this,
            android.os.Build.VERSION.SDK_INT < 33 ||
                checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED,
        )
        requestNotificationPermissionIfNeeded()
        // Reconcile notification permission changes made in system settings and
        // replay every retained Host registration after a background interval.
        model.refreshPushRegistration()
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == 1001) {
            LocalNotifications.permissionResult(
                this,
                grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED,
            )
        }
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean("continueOverInternet", continueOverInternet)
        super.onSaveInstanceState(outState)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        handleSurfaceIntent(intent)
        continueOverInternet = savedInstanceState?.getBoolean("continueOverInternet") ?: false
        intent?.let(model::openPushDeepLink)
        setContent {
            if (localNetworkGranted || continueOverInternet) {
                val requestQrScan = rememberAndroidQrScanner(this)
                RemoteAgentApp(
                    activity = this,
                    model = model,
                    requestQrScan = requestQrScan,
                    requestNotifications = {
                        requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                    },
                )
            } else {
                AppMaterialTheme {
                    Column(Modifier.safeDrawingPadding().padding(24.dp)) {
                        Text("同じネットワークのPCへ接続するには、付近のデバイスへのアクセスを許可してください。")
                        Button(onClick = { requestNetwork.launch(Manifest.permission.ACCESS_LOCAL_NETWORK) }) {
                            Text("許可して接続")
                        }
                        Text("拒否した場合は再試行するか、設定で許可できます。インターネット経由の接続も選べます。")
                        Button(
                            onClick = {
                                startActivity(
                                    Intent(
                                        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                                        Uri.parse("package:$packageName"),
                                    )
                                )
                            }
                        ) {
                            Text("アプリの設定を開く")
                        }
                        Button(onClick = { continueOverInternet = true }) { Text("インターネット経由で接続") }
                    }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        intent.let(model::openPushDeepLink)
        handleSurfaceIntent(intent)
    }

    private fun handleSurfaceIntent(intent: Intent) {
        when (intent.action) {
            Intent.ACTION_SEND -> {
                val text = intent.getStringExtra(Intent.EXTRA_TEXT) ?: intent.dataString.orEmpty()
                val urls = buildList {
                    intent.getStringExtra(Intent.EXTRA_STREAM)?.let(::add)
                    intent.clipData?.let { clip ->
                        repeat(clip.itemCount) { index -> clip.getItemAt(index).uri?.toString()?.let(::add) }
                    }
                }
                model.importShare(text, urls)
            }
            Intent.ACTION_VIEW if intent.data?.scheme == "remote-agent" -> {
                when (intent.data?.host) {
                    "new" -> model.newThreadFromShortcut()
                    "thread" -> intent.data?.pathSegments?.firstOrNull()?.let(model::openThread)
                    "share" -> {
                        val uri = intent.data ?: return
                        val text = uri.getQueryParameter("text").orEmpty()
                        val url = uri.getQueryParameter("url")
                        model.importShare(text, listOfNotNull(url))
                    }
                }
            }
        }
    }
}
