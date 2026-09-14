package dev.remoteagent.mobile

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner

/**
 * Reconnect the selected Host on foreground and persist on background.
 */
class AndroidConnectionLifecycle(private val onForeground: () -> Unit, private val onBackground: () -> Unit) :
    DefaultLifecycleObserver {
    override fun onStart(owner: LifecycleOwner) = onForeground()

    override fun onStop(owner: LifecycleOwner) = onBackground()
}
