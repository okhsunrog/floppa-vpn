package dev.okhsunrog.floppavpn.vpn

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest

/** Process-local network selection for Rust HTTP sockets, in both UI and VPN processes. */
object NetworkTransport {
    private var started = false

    private external fun nativeSetNetwork(handle: Long)

    @Synchronized
    fun start(context: Context) {
        if (started) return
        val manager = context.applicationContext.getSystemService(ConnectivityManager::class.java)
        fun update(excluded: Network? = null) {
            val candidates =
                manager.allNetworks.filter { network ->
                    val caps = manager.getNetworkCapabilities(network)
                    network != excluded &&
                        caps != null &&
                        caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
                        caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                }
            val preferred = candidates.maxByOrNull { network ->
                val caps = manager.getNetworkCapabilities(network) ?: return@maxByOrNull -1
                (if (caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)) 4 else 0) +
                    (if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) 2 else 0)
            }
            nativeSetNetwork(preferred?.networkHandle ?: 0L)
        }
        val callback =
            object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) = update()

                override fun onLost(network: Network) = update(network)

                override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) =
                    update()
            }
        manager.registerNetworkCallback(
            NetworkRequest.Builder()
                .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                .build(),
            callback,
        )
        started = true
        update()
    }
}
