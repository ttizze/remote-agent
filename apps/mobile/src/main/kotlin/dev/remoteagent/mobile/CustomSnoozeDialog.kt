package dev.remoteagent.mobile

import android.text.format.DateFormat
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.BasicAlertDialog
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TimePicker
import androidx.compose.material3.TimePickerDefaults
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.material3.rememberTimePickerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.CustomSnoozeInput
import dev.remoteagent.core.SnoozeDurationUnit
import dev.remoteagent.core.customSnoozeUntil
import java.time.Instant
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

private const val HOUR_MILLIS = 3_600_000L
private const val MAX_DURATION = 99
private const val DEFAULT_DURATION = 2

private enum class SnoozeMode(val label: String) {
    Date("Date and time"),
    Duration("Duration"),
}

private enum class PickerPart {
    Date,
    Time,
}

private val units =
    listOf(
        SnoozeDurationUnit.MINUTES to "Minutes",
        SnoozeDurationUnit.HOURS to "Hours",
        SnoozeDurationUnit.DAYS to "Days",
    )

@Composable
private fun <T> SegmentedChoice(options: List<Pair<T, String>>, selected: T, onSelect: (T) -> Unit) {
    val colors = AppTheme.colors
    SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
        options.forEachIndexed { index, (value, label) ->
            SegmentedButton(
                selected = value == selected,
                onClick = { onSelect(value) },
                shape = SegmentedButtonDefaults.itemShape(index, options.size),
                colors =
                    SegmentedButtonDefaults.colors(
                        activeContainerColor = colors.secondary,
                        activeContentColor = colors.secondaryForeground,
                        inactiveContainerColor = colors.cardAlt,
                        inactiveContentColor = colors.foreground,
                        activeBorderColor = colors.border,
                        inactiveBorderColor = colors.border,
                    ),
            ) {
                Text(label, style = AppTheme.footnote)
            }
        }
    }
}

/** The Material custom snooze dialog: a date and time, or a duration. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
@Suppress("LongMethod", "CyclomaticComplexMethod")
internal fun CustomSnoozeDialog(onClose: () -> Unit, onSnooze: (String) -> Unit) {
    val context = LocalContext.current
    val is24Hour = remember { DateFormat.is24HourFormat(context) }
    val zone = ZoneId.systemDefault()
    var mode by remember { mutableStateOf(SnoozeMode.Date) }
    var date by remember { mutableStateOf(LocalDateTime.ofInstant(Instant.now().plusMillis(HOUR_MILLIS), zone)) }
    var picker by remember { mutableStateOf(PickerPart.Date) }
    var amount by remember { mutableIntStateOf(DEFAULT_DURATION) }
    var unit by remember { mutableStateOf(SnoozeDurationUnit.HOURS) }
    var error by remember { mutableStateOf<String?>(null) }
    val colors = AppTheme.colors
    fun submit() {
        val input =
            if (mode == SnoozeMode.Date)
                CustomSnoozeInput.Date(
                    date.format(DateTimeFormatter.ISO_LOCAL_DATE),
                    date.format(DateTimeFormatter.ofPattern("HH:mm")),
                )
            else CustomSnoozeInput.Duration(amount.toString(), unit)
        val until = customSnoozeUntil(input, System.currentTimeMillis())
        if (until == null) {
            error =
                if (mode == SnoozeMode.Date) "Choose a date and time in the future." else "Enter a positive duration."
            return
        }
        onSnooze(until)
        onClose()
    }
    BasicAlertDialog(onDismissRequest = onClose, modifier = Modifier.widthIn(max = 360.dp).padding(16.dp)) {
        Surface(color = colors.cardAlt, contentColor = colors.foreground, shape = RoundedCornerShape(28.dp)) {
            Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState())) {
                Column(
                    Modifier.fillMaxWidth().padding(24.dp, 24.dp, 24.dp, 16.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    Text("Custom snooze", style = AppTheme.title)
                    SegmentedChoice(SnoozeMode.entries.map { it to it.label }, mode) {
                        mode = it
                        error = null
                    }
                    if (mode == SnoozeMode.Date)
                        SegmentedChoice(
                            listOf(
                                PickerPart.Date to date.format(DateTimeFormatter.ofPattern("MMM d")),
                                PickerPart.Time to
                                    date.format(DateTimeFormatter.ofPattern(if (is24Hour) "HH:mm" else "h:mm a")),
                            ),
                            picker,
                        ) {
                            picker = it
                        }
                }
                if (mode == SnoozeMode.Date) {
                    key(picker) {
                        if (picker == PickerPart.Date) {
                            val state =
                                rememberDatePickerState(
                                    initialSelectedDateMillis =
                                        date.toLocalDate().atStartOfDay().toInstant(ZoneOffset.UTC).toEpochMilli()
                                )
                            LaunchedEffect(state.selectedDateMillis) {
                                state.selectedDateMillis?.let { millis ->
                                    val day = Instant.ofEpochMilli(millis).atZone(ZoneOffset.UTC).toLocalDate()
                                    date = date.with(day)
                                    error = null
                                }
                            }
                            DatePicker(
                                state,
                                showModeToggle = false,
                                title = null,
                                headline = null,
                                colors = DatePickerDefaults.colors(containerColor = colors.cardAlt),
                            )
                        } else {
                            val state = rememberTimePickerState(date.hour, date.minute, is24Hour)
                            LaunchedEffect(state.hour, state.minute) {
                                date = date.withHour(state.hour).withMinute(state.minute)
                                error = null
                            }
                            Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
                                TimePicker(
                                    state,
                                    colors =
                                        TimePickerDefaults.colors(
                                            clockDialColor = colors.secondary,
                                            selectorColor = colors.primary,
                                        ),
                                )
                            }
                        }
                    }
                } else {
                    Column(
                        Modifier.fillMaxWidth().padding(24.dp, 8.dp, 24.dp, 16.dp),
                        verticalArrangement = Arrangement.spacedBy(16.dp),
                    ) {
                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.SpaceEvenly,
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            val buttonColors =
                                IconButtonDefaults.filledTonalIconButtonColors(
                                    containerColor = colors.secondary,
                                    contentColor = colors.secondaryForeground,
                                )
                            FilledTonalIconButton(
                                onClick = { amount = maxOf(1, amount - 1) },
                                enabled = amount > 1,
                                colors = buttonColors,
                                modifier = Modifier.size(48.dp),
                            ) {
                                Text("−", style = AppTheme.title)
                            }
                            Text(amount.toString(), style = AppTheme.title)
                            FilledTonalIconButton(
                                onClick = { amount = minOf(MAX_DURATION, amount + 1) },
                                enabled = amount < MAX_DURATION,
                                colors = buttonColors,
                                modifier = Modifier.size(48.dp),
                            ) {
                                Text("+", style = AppTheme.title)
                            }
                        }
                        SegmentedChoice(units, unit) { unit = it }
                    }
                }
                Column(
                    Modifier.fillMaxWidth().padding(24.dp, 8.dp, 24.dp, 24.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    error?.let { Text(it, style = AppTheme.footnote, color = colors.dangerForeground) }
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                        TextButton(onClick = onClose) {
                            Text("Cancel", style = AppTheme.footnote, color = colors.foreground)
                        }
                        TextButton(onClick = ::submit) {
                            Text("Snooze", style = AppTheme.footnote, color = colors.foreground)
                        }
                    }
                }
            }
        }
    }
}
