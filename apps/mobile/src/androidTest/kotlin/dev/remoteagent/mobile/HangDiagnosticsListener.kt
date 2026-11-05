package dev.remoteagent.mobile

import android.os.Process
import android.util.Log
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import org.junit.runner.Description
import org.junit.runner.Result
import org.junit.runner.notification.RunListener

class HangDiagnosticsListener : RunListener() {
    private val scheduler = Executors.newSingleThreadScheduledExecutor { task ->
        Thread(task, "fixture-hang-diagnostics").apply { isDaemon = true }
    }
    private var pending: ScheduledFuture<*>? = null

    private fun arm(label: String) {
        pending?.cancel(false)
        pending =
            scheduler.schedule(
                {
                    Log.e("TestRunner", "Hang diagnostics: $label")
                    Thread.getAllStackTraces().forEach { (thread, stack) ->
                        Log.e("TestRunner", "${thread.name}: ${thread.state}\n${stack.joinToString("\n")}")
                    }
                    Process.killProcess(Process.myPid())
                },
                60,
                TimeUnit.SECONDS,
            )
    }

    override fun testRunStarted(description: Description) {
        arm("before first test")
    }

    override fun testStarted(description: Description) {
        arm("running ${description.displayName}")
    }

    override fun testFinished(description: Description) {
        arm("after ${description.displayName}")
    }

    override fun testRunFinished(result: Result) {
        pending?.cancel(false)
        scheduler.shutdownNow()
    }
}
