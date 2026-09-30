package dev.remoteagent.mobile

import android.annotation.SuppressLint
import android.view.ViewGroup
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.LoadVisualization
import dev.remoteagent.core.Outcome
import java.io.ByteArrayInputStream
import kotlin.coroutines.resume
import kotlinx.coroutines.suspendCancellableCoroutine

@Composable
internal fun ConversationVisualization(
    path: String,
    cwd: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
) {
    var html by remember(path, cwd) { mutableStateOf<String?>(null) }
    var error by remember(path, cwd) { mutableStateOf<String?>(null) }
    LaunchedEffect(path, cwd) {
        if (perform == null) {
            error = "Hostに接続して表示を読み込んでください。"
        } else {
            val result =
                suspendCancellableCoroutine<Result<Outcome>> { continuation ->
                    perform(Intent.LoadVisualization(LoadVisualization(path, cwd))) {
                        if (continuation.isActive) continuation.resume(it)
                    }
                }
            val outcome = result.getOrNull()
            if (outcome is Outcome.Visualization) html = outcome.html
            else error = result.exceptionOrNull()?.message ?: "表示の応答が無効です。"
        }
    }
    val document = html
    if (document != null) VisualizationWebView(document) else Text(error?.let { "表示できません: $it" } ?: "インタラクティブ表示を読み込み中…")
}

@SuppressLint("SetJavaScriptEnabled")
@Composable
internal fun VisualizationWebView(html: String) {
    AndroidView(
        modifier = Modifier.fillMaxWidth().height(560.dp),
        factory = { context ->
            WebView(context).apply {
                layoutParams =
                    ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
                settings.javaScriptEnabled = true
                settings.javaScriptCanOpenWindowsAutomatically = false
                settings.allowFileAccess = false
                settings.allowContentAccess = false
                settings.blockNetworkLoads = true
                settings.domStorageEnabled = false
                settings.mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
                webViewClient =
                    object : WebViewClient() {
                        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) =
                            request.url.scheme != "about"

                        // WebView uses internal data: requests for inline documents.
                        override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest) =
                            if (request.url.scheme in listOf("about", "data", "blob")) null
                            else WebResourceResponse("text/plain", "UTF-8", ByteArrayInputStream(ByteArray(0)))
                    }
            }
        },
        update = { view ->
            if (view.tag != html) {
                view.tag = html
                view.loadDataWithBaseURL(null, html, "text/html", "UTF-8", null)
            }
        },
        onRelease = {
            it.stopLoading()
            it.destroy()
        },
    )
}
