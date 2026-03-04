package dev.okhsunrog.floppavpn.vpn

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.core.app.NotificationCompat

/**
 * Android VpnService implementation for Floppa VPN.
 *
 * Runs in a separate `:vpn` process (android:process=":vpn" in manifest).
 * Creates a TUN interface and delegates WireGuard tunnel management to Rust
 * via JNI. The Rust code runs a tarpc RPC server for the UI process to
 * query status, stats, and request disconnect.
 */
class FloppaVpnService : VpnService() {

    companion object {
        private const val TAG = "FloppaVpnService"
        private const val NOTIFICATION_CHANNEL_ID = "vpn_service"
        private const val NOTIFICATION_ID = 1

        /** Action to stop the VPN service from another process (UI → :vpn) */
        const val ACTION_STOP = "dev.okhsunrog.floppavpn.STOP_VPN"

        // Intent extras
        const val EXTRA_IPV4_ADDR = "ipv4_addr"
        const val EXTRA_IPV6_ADDR = "ipv6_addr"
        const val EXTRA_ROUTES = "routes"
        const val EXTRA_DNS = "dns"
        const val EXTRA_MTU = "mtu"
        const val EXTRA_DISALLOWED_APPS = "disallowed_apps"
        const val EXTRA_ALLOWED_APPS = "allowed_apps"
        const val EXTRA_WG_CONFIG = "wg_config"

        // Singleton instance for local protectSocket() calls from JNI
        @JvmField
        var instance: FloppaVpnService? = null

        init {
            System.loadLibrary("floppa_client_lib")
        }
    }

    // Native methods implemented in Rust (vpn/jni_entry.rs)
    private external fun nativeInit()
    private external fun nativeStartTunnel(tunFd: Int, wgConfig: String, socketPath: String)
    private external fun nativeStop()

    private var tunInterface: ParcelFileDescriptor? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
        createNotificationChannel()
        nativeInit()
        Log.i(TAG, "VPN service created (separate :vpn process)")
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Log.i(TAG, "onStartCommand: action=${intent?.action}")

        if (intent == null) {
            Log.w(TAG, "Null intent, stopping service")
            stopSelf()
            return START_NOT_STICKY
        }

        // Handle stop request from UI process
        if (intent.action == ACTION_STOP) {
            Log.i(TAG, "Received STOP action, shutting down")
            nativeStop()
            cleanupAndroid()
            instance = null
            stopSelf()
            return START_NOT_STICKY
        }

        val wgConfig = intent.getStringExtra(EXTRA_WG_CONFIG)
        if (wgConfig == null) {
            Log.e(TAG, "Missing WG config in intent")
            stopSelf()
            return START_NOT_STICKY
        }

        // Start as foreground service immediately (before TUN creation)
        // to satisfy Android's foreground service requirements
        startVpnForeground()

        try {
            tunInterface = createTunInterface(intent)
            val fd = tunInterface?.fd ?: throw IllegalStateException("Failed to get TUN fd")

            Log.i(TAG, "TUN interface created with fd: $fd")

            // Start the WireGuard tunnel and tarpc RPC server via JNI
            val socketPath = applicationInfo.dataDir + "/vpn.sock"
            nativeStartTunnel(fd, wgConfig, socketPath)

        } catch (e: Exception) {
            Log.e(TAG, "Failed to start VPN tunnel", e)
            stopSelf()
            return START_NOT_STICKY
        }

        return START_STICKY
    }

    override fun onDestroy() {
        Log.i(TAG, "VPN service destroying")
        // onDestroy is called by Android when the service is being torn down
        // (e.g., system kill). Stop Rust side and clean up.
        nativeStop()
        cleanupAndroid()
        instance = null
        super.onDestroy()
    }

    override fun onRevoke() {
        Log.i(TAG, "VPN permission revoked")
        nativeStop()
        cleanupAndroid()
        instance = null
        super.onRevoke()
    }

    /**
     * Clean up Android-side resources (TUN, foreground notification) and stop the service.
     * Called from Rust via JNI after the tunnel and RPC server are already stopped,
     * and from onDestroy/onRevoke for system-initiated shutdowns.
     */
    fun shutdownService() {
        Log.i(TAG, "shutdownService() called")
        cleanupAndroid()
        stopSelf()
    }

    private fun cleanupAndroid() {
        stopForeground(STOP_FOREGROUND_REMOVE)

        tunInterface?.let { tun ->
            Log.i(TAG, "Closing TUN interface")
            try {
                tun.close()
            } catch (e: Exception) {
                Log.w(TAG, "Error closing TUN interface", e)
            }
            tunInterface = null
        }
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                NOTIFICATION_CHANNEL_ID,
                "VPN Service",
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "Shows when VPN is active"
            }
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(channel)
        }
    }

    private fun startVpnForeground() {
        val notification = NotificationCompat.Builder(this, NOTIFICATION_CHANNEL_ID)
            .setContentTitle("Floppa VPN")
            .setContentText("Connected")
            .setSmallIcon(android.R.drawable.ic_lock_lock)
            .setOngoing(true)
            .setContentIntent(createOpenAppIntent())
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(
                NOTIFICATION_ID, notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SYSTEM_EXEMPTED
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    /**
     * Create a PendingIntent that opens the app when the notification is tapped.
     */
    private fun createOpenAppIntent(): PendingIntent {
        val intent = packageManager.getLaunchIntentForPackage(packageName)
            ?: Intent()
        intent.flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED
        return PendingIntent.getActivity(
            this, 0, intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
    }

    private fun createTunInterface(intent: Intent): ParcelFileDescriptor {
        val ipv4Addr = intent.getStringExtra(EXTRA_IPV4_ADDR) ?: "10.0.0.2/24"
        val ipv6Addr = intent.getStringExtra(EXTRA_IPV6_ADDR)
        val routes = intent.getStringArrayExtra(EXTRA_ROUTES) ?: emptyArray()
        val dns = intent.getStringExtra(EXTRA_DNS)
        val mtu = intent.getIntExtra(EXTRA_MTU, 1280)
        val disallowedApps = intent.getStringArrayExtra(EXTRA_DISALLOWED_APPS) ?: emptyArray()
        val allowedApps = intent.getStringArrayExtra(EXTRA_ALLOWED_APPS) ?: emptyArray()

        Log.i(TAG, "Creating TUN: ipv4=$ipv4Addr, ipv6=$ipv6Addr, mtu=$mtu, routes=${routes.size}, dns=$dns")

        val builder = Builder()
            .setSession("Floppa VPN")
            .setMtu(mtu)
            .setBlocking(false)

        // Add IPv4 address
        val (ipv4, prefix4) = parseAddress(ipv4Addr)
        builder.addAddress(ipv4, prefix4)

        // Add IPv6 address if provided
        ipv6Addr?.let {
            val (ipv6, prefix6) = parseAddress(it)
            builder.addAddress(ipv6, prefix6)
        }

        // Add routes
        for (route in routes) {
            try {
                val (addr, prefix) = parseAddress(route)
                builder.addRoute(addr, prefix)
            } catch (e: Exception) {
                Log.w(TAG, "Invalid route: $route", e)
            }
        }

        // Add DNS servers (may be comma-separated, e.g. "1.1.1.1, 8.8.8.8")
        dns?.let {
            val servers = it.split(",").map { server -> server.trim() }
                .filter { server -> server.isNotEmpty() }
            for (server in servers) {
                try {
                    builder.addDnsServer(server)
                } catch (e: Exception) {
                    Log.w(TAG, "Invalid DNS server: $server", e)
                }
            }
        }

        // Split tunneling: allowed and disallowed are mutually exclusive in Android VPN API.
        // If allowedApps is set, only those apps go through VPN.
        // If disallowedApps is set, all apps except those go through VPN.
        if (allowedApps.isNotEmpty()) {
            for (app in allowedApps) {
                try {
                    builder.addAllowedApplication(app)
                } catch (e: Exception) {
                    Log.w(TAG, "Cannot include app: $app", e)
                }
            }
        } else {
            for (app in disallowedApps) {
                try {
                    builder.addDisallowedApplication(app)
                } catch (e: Exception) {
                    Log.w(TAG, "Cannot exclude app: $app", e)
                }
            }
        }

        // Set as non-metered on Android 10+
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            builder.setMetered(false)
        }

        return builder.establish()
            ?: throw IllegalStateException("VpnService.Builder.establish() returned null")
    }

    private fun parseAddress(cidr: String): Pair<String, Int> {
        val parts = cidr.split("/")
        if (parts.size != 2) {
            throw IllegalArgumentException("Invalid CIDR notation: $cidr")
        }
        return Pair(parts[0], parts[1].toInt())
    }

    /**
     * Protect a socket from VPN routing.
     * Called from Rust JNI to ensure UDP sockets bypass the VPN,
     * preventing routing loops.
     */
    fun protectSocket(socket: Int): Boolean {
        return protect(socket)
    }
}
