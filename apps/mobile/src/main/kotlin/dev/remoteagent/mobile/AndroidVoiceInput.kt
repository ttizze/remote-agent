package dev.remoteagent.mobile

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent as AndroidIntent
import android.speech.RecognizerIntent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.painterResource
import dev.remoteagent.core.ComposerPresentation
import dev.remoteagent.core.DraftKey
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SessionRef
import java.util.Locale

// Recognition runs in the phone's speech activity, which owns microphone permission.
@Composable
internal fun VoiceInputButton(
    composer: ComposerPresentation,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    enabled: Boolean,
) {
    val language = LocalUiLanguage.current
    val currentPerform by rememberUpdatedState(perform)
    var selection by
        rememberSaveable(stateSaver = voiceDraftKeySaver) { mutableStateOf(composer.controls().navigation.draftKey) }
    var pending by rememberSaveable { mutableStateOf(false) }
    var error by rememberSaveable { mutableStateOf<String?>(null) }
    val recognition =
        rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            val requested = pending
            pending = false
            if (requested && result.resultCode == Activity.RESULT_OK) {
                val spoken =
                    result.data?.getStringArrayListExtra(RecognizerIntent.EXTRA_RESULTS)?.firstOrNull {
                        it.isNotBlank()
                    }
                if (spoken != null) {
                    currentPerform(Intent.AppendTranscript(selection, spoken)) {}
                } else error = language.text("音声を認識できませんでした。もう一度お試しください。")
            } else if (requested && result.resultCode != Activity.RESULT_CANCELED) {
                error = language.text("音声入力に失敗しました。端末の音声認識の設定や接続を確認してください。")
            }
        }
    IconButton(
        onClick = {
            selection = composer.controls().navigation.draftKey
            pending = true
            try {
                recognition.launch(voiceRecognitionIntent(language.text("AIへの依頼を話してください")))
            } catch (_: ActivityNotFoundException) {
                pending = false
                error = language.text("音声認識アプリが見つかりません。端末の音声入力を設定してください。")
            } catch (_: SecurityException) {
                pending = false
                error = language.text("音声入力を開始できません。端末の音声認識アプリの権限を確認してください。")
            }
        },
        enabled = enabled && !pending,
    ) {
        Icon(painterResource(R.drawable.ic_microphone), contentDescription = language.text("音声入力"))
    }
    error?.let { message ->
        AlertDialog(
            onDismissRequest = { error = null },
            confirmButton = { TextButton(onClick = { error = null }) { Text(language.text("閉じる")) } },
            title = { Text(language.text("音声入力")) },
            text = { Text(message) },
        )
    }
}

private fun voiceRecognitionIntent(prompt: String) =
    AndroidIntent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
        .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
        .putExtra(RecognizerIntent.EXTRA_LANGUAGE, Locale.getDefault().toLanguageTag())
        .putExtra(RecognizerIntent.EXTRA_PROMPT, prompt)
        .putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1)

private val voiceDraftKeySaver =
    Saver<DraftKey, List<String>>(
        save = { key ->
            when (key) {
                is DraftKey.Local -> listOf("local", key.key)
                is DraftKey.Session -> listOf(key.session.provider?.name ?: "bex", key.session.id)
            }
        },
        restore = { parts ->
            if (parts[0] == "local") DraftKey.Local(parts[1])
            else DraftKey.Session(SessionRef(if (parts[0] == "bex") null else ProviderKind.valueOf(parts[0]), parts[1]))
        },
    )
