package dev.remoteagent.mobile

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent

class MainActivity : ComponentActivity() {
    private lateinit var repository: AndroidMobileRepository
    private lateinit var gateway: AndroidHostGateway

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        repository = AndroidMobileRepository(applicationContext)
        gateway = AndroidHostGateway(applicationContext)
        setContent {
            val requestQrScan = rememberAndroidQrScanner(this)
            RemoteAgentApp(gateway = gateway, repository = repository, requestQrScan = requestQrScan)
        }
    }
}
