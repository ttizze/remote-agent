package dev.remoteagent.mobile

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner

/**
 * Foreground connection hook. The owner decides whether background should release the transport; this milestone
 * reconnects the selected Host on start without claiming a false connected state while reconnecting.
 */
class AndroidConnectionLifecycle(private val onForeground: () -> Unit, private val onBackground: () -> Unit) :
    DefaultLifecycleObserver {
    override fun onStart(owner: LifecycleOwner) = onForeground()

    override fun onStop(owner: LifecycleOwner) = onBackground()
}
