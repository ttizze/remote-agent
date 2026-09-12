package dev.remoteagent.mobile

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner

/**
 * Refresh the selected Host on foreground and persist on background, retaining a live connection.
 */
class AndroidConnectionLifecycle(private val onForeground: () -> Unit, private val onBackground: () -> Unit) :
    DefaultLifecycleObserver {
    override fun onStart(owner: LifecycleOwner) = onForeground()

    override fun onStop(owner: LifecycleOwner) = onBackground()
}
