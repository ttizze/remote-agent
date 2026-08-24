package dev.remoteagent.mobile

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import java.net.Inet6Address

/** Resolves locally advertised Host Daemon endpoints; it never changes host trust. */
class AndroidMdnsDiscovery(context: Context) {
    private val nsd = context.applicationContext.getSystemService(NsdManager::class.java)
    private var listener: NsdManager.DiscoveryListener? = null

    fun start(onEndpoint: (String) -> Unit, onError: (Int) -> Unit = {}) {
        stop()
        listener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String) = Unit
            override fun onDiscoveryStopped(serviceType: String) = Unit
            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                if (serviceInfo.serviceType.equals(ServiceType, ignoreCase = true)) resolve(serviceInfo, onEndpoint, onError)
            }
            override fun onServiceLost(serviceInfo: NsdServiceInfo) = Unit
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) { stop(); onError(errorCode) }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) { stop(); onError(errorCode) }
        }.also { nsd.discoverServices(ServiceType, NsdManager.PROTOCOL_DNS_SD, it) }
    }

    fun stop() {
        listener?.let {
            runCatching { nsd.stopServiceDiscovery(it) }
            listener = null
        }
    }

    private fun resolve(service: NsdServiceInfo, onEndpoint: (String) -> Unit, onError: (Int) -> Unit) {
        nsd.resolveService(service, object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) = onError(errorCode)
            override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                val host = serviceInfo.host ?: return
                val address = host.hostAddress ?: return
                val renderedHost = if (host is Inet6Address) "[$address]" else address
                onEndpoint("$renderedHost:${serviceInfo.port}")
            }
        })
    }

    private companion object { const val ServiceType = "_bex._udp." }
}
