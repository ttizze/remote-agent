package dev.remoteagent.mobile

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Button
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory

private const val LOCAL_NETWORK_PERMISSION_API = 37

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

    override fun onResume() {
        super.onResume()
        localNetworkGranted =
            !requiresLocalNetworkPermission(Build.VERSION.SDK_INT) ||
                checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK) == PackageManager.PERMISSION_GRANTED
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean("continueOverInternet", continueOverInternet)
        super.onSaveInstanceState(outState)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        continueOverInternet = savedInstanceState?.getBoolean("continueOverInternet") ?: false
        setContent {
            if (localNetworkGranted || continueOverInternet) {
                val requestQrScan = rememberAndroidQrScanner(this)
                RemoteAgentApp(activity = this, model = model, requestQrScan = requestQrScan)
            } else {
                BexTheme {
                    Surface(Modifier.fillMaxSize()) {
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
    }
}

/** Local-network runtime permission exists only from Android 17 onward. */
internal fun requiresLocalNetworkPermission(sdk: Int): Boolean = sdk >= LOCAL_NETWORK_PERMISSION_API
