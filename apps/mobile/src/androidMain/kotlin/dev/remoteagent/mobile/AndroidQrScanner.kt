package dev.remoteagent.mobile

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/** CameraX/ML Kit QR flow. Only decoded text is returned; camera frames are never persisted. */
@Composable
fun rememberAndroidQrScanner(activity: ComponentActivity): (onContents: (String) -> Unit) -> Unit {
    var request by remember { mutableStateOf<((String) -> Unit)?>(null) }
    var permissionRequested by remember { mutableStateOf(false) }
    val permission =
        rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            permissionRequested = granted
            if (!granted) request = null
        }
    val requestScan: ((String) -> Unit) -> Unit = { callback ->
        request = callback
        permissionRequested =
            ContextCompat.checkSelfPermission(activity, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
        if (!permissionRequested) permission.launch(Manifest.permission.CAMERA)
    }
    if (request != null && permissionRequested) {
        AndroidQrCaptureDialog(
            activity = activity,
            onContents = { contents ->
                request?.invoke(contents)
                request = null
            },
            onDismiss = { request = null },
        )
    }
    return requestScan
}

@Composable
private fun AndroidQrCaptureDialog(activity: ComponentActivity, onContents: (String) -> Unit, onDismiss: () -> Unit) {
    val executor = remember { Executors.newSingleThreadExecutor() }
    val handled = remember { AtomicBoolean(false) }
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = { TextButton(onClick = onDismiss) { Text("キャンセル") } },
        title = { Text("QRコードを読み取る") },
        text = {
            AndroidView(
                factory = { viewContext ->
                    PreviewView(viewContext).also { previewView ->
                        bindCamera(activity, previewView, executor, handled, onContents)
                    }
                },
                modifier = Modifier.fillMaxSize(),
            )
        },
    )
    androidx.compose.runtime.DisposableEffect(Unit) { onDispose { executor.shutdownNow() } }
}

private fun bindCamera(
    activity: ComponentActivity,
    previewView: PreviewView,
    executor: ExecutorService,
    handled: AtomicBoolean,
    onContents: (String) -> Unit,
) {
    val providerFuture = ProcessCameraProvider.getInstance(activity)
    providerFuture.addListener(
        {
            val provider = runCatching { providerFuture.get() }.getOrNull() ?: return@addListener
            val scannerOptions = BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build()
            val scanner = BarcodeScanning.getClient(scannerOptions)
            val analysis =
                ImageAnalysis.Builder().build().also { useCase ->
                    useCase.setAnalyzer(executor) { imageProxy ->
                        val mediaImage = imageProxy.image
                        if (mediaImage == null || handled.get()) {
                            imageProxy.close()
                        } else {
                            scanner
                                .process(InputImage.fromMediaImage(mediaImage, imageProxy.imageInfo.rotationDegrees))
                                .addOnSuccessListener(executor) { barcodes ->
                                    val value = barcodes.firstOrNull { it.format == Barcode.FORMAT_QR_CODE }?.rawValue
                                    if (value != null && handled.compareAndSet(false, true))
                                        activity.runOnUiThread { onContents(value) }
                                }
                                .addOnCompleteListener { imageProxy.close() }
                        }
                    }
                }
            runCatching {
                    provider.unbindAll()
                    provider.bindToLifecycle(
                        activity,
                        CameraSelector.DEFAULT_BACK_CAMERA,
                        Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) },
                        analysis,
                    )
                }
                .onFailure { scanner.close() }
        },
        ContextCompat.getMainExecutor(activity),
    )
}
