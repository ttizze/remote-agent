package dev.remoteagent.mobile

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory

class MainActivity : ComponentActivity() {
    private val model: AndroidAppModel by viewModels {
        viewModelFactory { initializer { AndroidAppModel(applicationContext) } }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            val requestQrScan = rememberAndroidQrScanner(this)
            RemoteAgentApp(activity = this, model = model, requestQrScan = requestQrScan)
        }
    }
}
