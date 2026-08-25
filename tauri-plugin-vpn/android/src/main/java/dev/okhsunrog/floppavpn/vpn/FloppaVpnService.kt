package dev.okhsunrog.floppavpn.vpn

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.core.app.NotificationCompat
import java.io.File
import org.json.JSONArray
import org.json.JSONObject

/**
 * What `VpnService.Builder` is given, from whichever of the two sources started this instance: the
 * plugin's start intent, or the autostart bundle Rust hands back for a start the system issued.
 *
 * The address, routes and MTU are required. The plugin always sends them, the bundle always holds
 * them, and a TUN built from a made-up default would carry a default route into a tunnel nobody
 * configured — so anything missing is an error, not a fallback.
 */
data class TunSpec(
    /** Generation this instance serves; echoed over the RPC and matched on teardown. */
    val epoch: Long,
    val ipv4Addr: String,
    val ipv6Addr: String?,
    val routes: List<String>,
    val dns: String?,
    val mtu: Int,
    val disallowedApps: List<String>,
    val allowedApps: List<String>,
) {
    init {
        require(mtu > 0) { "invalid MTU: $mtu" }
    }

    companion object {
        fun fromIntent(intent: Intent): TunSpec {
            if (!intent.hasExtra(FloppaVpnService.EXTRA_MTU)) {
                throw IllegalArgumentException("start intent has no ${FloppaVpnService.EXTRA_MTU}")
            }
            return TunSpec(
                epoch = intent.getLongExtra(FloppaVpnService.EXTRA_EPOCH, 0L),
                ipv4Addr =
                    intent.getStringExtra(FloppaVpnService.EXTRA_IPV4_ADDR)
                        ?: throw IllegalArgumentException(
                            "start intent has no ${FloppaVpnService.EXTRA_IPV4_ADDR}"
                        ),
                ipv6Addr = intent.getStringExtra(FloppaVpnService.EXTRA_IPV6_ADDR),
                routes =
                    intent.getStringArrayExtra(FloppaVpnService.EXTRA_ROUTES)?.toList()
                        ?: throw IllegalArgumentException(
                            "start intent has no ${FloppaVpnService.EXTRA_ROUTES}"
                        ),
                dns = intent.getStringExtra(FloppaVpnService.EXTRA_DNS),
                mtu = intent.getIntExtra(FloppaVpnService.EXTRA_MTU, 0),
                disallowedApps =
                    intent.getStringArrayExtra(FloppaVpnService.EXTRA_DISALLOWED_APPS)?.toList()
                        ?: emptyList(),
                allowedApps =
                    intent.getStringArrayExtra(FloppaVpnService.EXTRA_ALLOWED_APPS)?.toList()
                        ?: emptyList(),
            )
        }

        /**
         * The JSON `nativeLoadAutostart` returns: the plugin's start payload, with the same field
         * names the intent extras use (`TunSpec::with_epoch` in `vpn/autostart.rs`).
         */
        fun fromJson(json: String): TunSpec {
            val o = JSONObject(json)
            return TunSpec(
                epoch = o.getLong("epoch"),
                ipv4Addr = o.getString("ipv4Addr"),
                ipv6Addr = o.stringOrNull("ipv6Addr"),
                routes = o.getJSONArray("routes").toStringList(),
                dns = o.stringOrNull("dns"),
                mtu = o.getInt("mtu"),
                disallowedApps = o.optJSONArray("disallowedApps")?.toStringList() ?: emptyList(),
                allowedApps = o.optJSONArray("allowedApps")?.toStringList() ?: emptyList(),
            )
        }

        /** `optString` would return the string "null" for a JSON null; this returns null. */
        private fun JSONObject.stringOrNull(key: String): String? =
            if (isNull(key)) null else getString(key)

        private fun JSONArray.toStringList(): List<String> = List(length()) { getString(it) }
    }
}

/**
 * Android VpnService implementation for Floppa VPN.
 *
 * Runs in a separate `:vpn` process (android:process=":vpn" in manifest). Creates a TUN interface
 * and delegates tunnel management (WireGuard or VLESS) to Rust via JNI. The Rust code runs a tarpc
 * RPC server for the UI process to query status, stats, and request disconnect.
 *
 * Two ways in. The plugin starts it with a configuration in the intent and then asks for the tunnel
 * over the RPC. The system starts it with no configuration — for always-on VPN, at boot, or to
 * restore a lockdown session — and then the service rebuilds the tunnel the last successful connect
 * wrote down (`autostart.json`, see `vpn/autostart.rs`) with no UI process involved.
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
        /**
         * Generation of the request that started this service.
         *
         * Echoed back over the RPC so a reply from an instance that has since been superseded is
         * rejectable by value, rather than by guessing from timing.
         */
        const val EXTRA_EPOCH = "epoch"

        init {
            System.loadLibrary("floppa_client_lib")
        }
    }

    // Native methods implemented in Rust (vpn/jni_entry.rs)
    private external fun nativeInit(logDir: String)

    /** Binds the RPC socket. Throws [RuntimeException] when it cannot. */
    private external fun nativeStartServer(tunFd: Int, socketPath: String, epoch: Long)

    /**
     * For a start the system issued: the TUN to build from the autostart bundle, as the JSON
     * [TunSpec.fromJson] reads, with a fresh epoch — or null when there is nothing to restore.
     */
    private external fun nativeLoadAutostart(dataDir: String): String?

    /**
     * After [nativeStartServer] on an autonomous start: bring the tunnel up from what
     * [nativeLoadAutostart] prepared. Throws [RuntimeException] when there is nothing to start
     * from; a start that fails later stops the service from the Rust side.
     */
    private external fun nativeStartTunnelFromBundle(epoch: Long)

    /**
     * Generation this instance was started with.
     *
     * Passed back on teardown so a late onDestroy from a previous instance cannot stop the server
     * belonging to the one that replaced it — these instances share a process, and stopService is
     * asynchronous.
     */
    private var epoch: Long = 0

    private external fun nativeStop(epoch: Long)

    private var tunInterface: ParcelFileDescriptor? = null

    /** Whether this generation was started by the system rather than by the plugin. */
    private var autonomous = false

    /**
     * Every field above is touched on the main thread only. Rust calls [shutdownService] and
     * [setConnected] from tokio worker threads, so those hop here first.
     */
    private val mainHandler = Handler(Looper.getMainLooper())

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        val logDir = File(applicationInfo.dataDir, "logs")
        logDir.mkdirs()
        nativeInit(logDir.absolutePath)
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
            nativeStop(epoch)
            cleanupAndroid()
            stopSelf()
            return START_NOT_STICKY
        }

        // The system starts this service with `Intent(action = android.net.VpnService)` and no
        // extras — for always-on VPN, at boot, or to restore a lockdown ("block connections
        // without VPN") session. Nothing about the tunnel travels in that intent, so it is
        // rebuilt from the bundle the last successful connect wrote. Without one there is nothing
        // to restore: refuse before startForeground so no notification is shown for it.
        //
        // The service is a singleton per process, so such an intent can also land on an instance
        // that is carrying a live tunnel. That tunnel is left exactly as it is: stopSelf() here
        // would run onDestroy, whose nativeStop(epoch) matches this generation and tears it
        // down. Only an instance with nothing to keep alive acts on the intent.
        if (intent.action == SERVICE_INTERFACE || !intent.hasExtra(EXTRA_EPOCH)) {
            if (tunInterface != null) {
                Log.w(
                    TAG,
                    "Start intent is not from the plugin (action=${intent.action}); a tunnel is up, ignoring",
                )
                return START_NOT_STICKY
            }
            return startFromBundle()
        }

        // A second start while a TUN is still established. The plugin's stopService() before a
        // start is asynchronous, and a start that arrives first is delivered to this same
        // instance. Tear the previous generation down before its descriptor is overwritten —
        // otherwise the old fd leaked with the old tunnel still reading from it.
        if (tunInterface != null) {
            Log.w(TAG, "Start while a tunnel is established; stopping the previous one first")
            nativeStop(epoch)
            cleanupAndroid()
        }

        val spec =
            try {
                TunSpec.fromIntent(intent)
            } catch (e: IllegalArgumentException) {
                // Nothing is foreground or established yet, so there is nothing to undo.
                Log.e(TAG, "Refusing a start intent without a usable configuration", e)
                stopSelf()
                return START_NOT_STICKY
            }
        // Bind the RPC server and stop there. The tunnel is started by a separate typed request
        // over that socket, so a failed start is reportable instead of looking like a service
        // that never came up.
        return startGeneration(spec, autonomous = false) {}
    }

    /**
     * A start the system issued: rebuild the last-good tunnel with no UI process.
     *
     * Rust reads the bundle and says what TUN to build; the tunnel is then brought up in this
     * process, on the same path the RPC start uses, under an epoch from a range no UI intent can
     * mint. A UI process that opens later finds it over the RPC — with its protocol and split rules
     * reported by this side — and adopts it.
     */
    private fun startFromBundle(): Int {
        val plan =
            try {
                nativeLoadAutostart(applicationInfo.dataDir)
            } catch (e: Exception) {
                Log.e(TAG, "Failed to read the autostart bundle", e)
                null
            }
        if (plan == null) {
            Log.w(TAG, "System start with nothing to restore (no autostart bundle); stopping")
            stopSelf()
            return START_NOT_STICKY
        }
        val spec =
            try {
                TunSpec.fromJson(plan)
            } catch (e: Exception) {
                Log.e(TAG, "The autostart bundle does not describe a usable TUN", e)
                stopSelf()
                return START_NOT_STICKY
            }
        Log.i(TAG, "System start: rebuilding the last-good tunnel (epoch=${spec.epoch})")
        return startGeneration(spec, autonomous = true) { nativeStartTunnelFromBundle(spec.epoch) }
    }

    /**
     * Go foreground, establish the TUN, bind the RPC, then run [afterBind] — the one step that
     * differs between a plugin start (nothing: the tunnel is requested over the socket) and an
     * autonomous one (start the tunnel from the bundle). Any failure along the way tears down what
     * was applied and stops the service.
     */
    private fun startGeneration(spec: TunSpec, autonomous: Boolean, afterBind: () -> Unit): Int {
        // Before anything that can fail or tear down: onDestroy stops by this value, and reading
        // it later meant a start that threw left the previous instance's generation in the field.
        epoch = spec.epoch
        this.autonomous = autonomous

        // Foreground before the TUN exists, because Android requires it — so the notification says
        // what is true at this point, not what we hope for. `setConnected` promotes it once the
        // tunnel is actually carrying traffic.
        startVpnForeground(connected = false)

        try {
            tunInterface = createTunInterface(spec)
            val fd = tunInterface?.fd ?: throw IllegalStateException("Failed to get TUN fd")

            Log.i(TAG, "TUN interface created with fd: $fd")

            // Keep in sync with SOCKET_NAME in rpc.rs.
            val socketPath = applicationInfo.dataDir + "/vpn.sock"
            nativeStartServer(fd, socketPath, epoch)
            afterBind()
        } catch (e: Exception) {
            // nativeStartServer throws when the socket cannot be bound. Whatever failed, the
            // service is foreground and may be holding an established TUN with a default route
            // into it; without the RPC nothing can ask it to stop, so it must not stay.
            Log.e(TAG, "Failed to start VPN service", e)
            nativeStop(epoch)
            cleanupAndroid()
            stopSelf()
            return START_NOT_STICKY
        }

        // Not sticky: a restart arrives with a null intent, and the null branch above can only
        // stop again. For always-on VPN it is the system that restarts the service, with the
        // VpnService action, and that start rebuilds from the bundle.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        Log.i(TAG, "VPN service destroying")
        // onDestroy is called by Android when the service is being torn down
        // (e.g., system kill). Stop Rust side and clean up.
        nativeStop(epoch)
        cleanupAndroid()
        super.onDestroy()
    }

    override fun onRevoke() {
        Log.i(TAG, "VPN permission revoked")
        nativeStop(epoch)
        cleanupAndroid()
        super.onRevoke()
    }

    /**
     * Clean up Android-side resources (TUN, foreground notification) and stop the service.
     *
     * Called from Rust via JNI, on a tokio thread, from the RPC `stop` handler after the tunnel is
     * already stopped. onDestroy then runs nativeStop for this generation, which releases the RPC
     * server and the service reference.
     */
    fun shutdownService() {
        Log.i(TAG, "shutdownService() called")
        mainHandler.post {
            cleanupAndroid()
            stopSelf()
        }
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
            val channel =
                NotificationChannel(
                        NOTIFICATION_CHANNEL_ID,
                        "VPN Service",
                        NotificationManager.IMPORTANCE_LOW,
                    )
                    .apply { description = "Shows when VPN is active" }
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(connected: Boolean): Notification {
        val state = if (connected) "Connected" else "Connecting\u2026"
        return NotificationCompat.Builder(this, NOTIFICATION_CHANNEL_ID)
            .setContentTitle(if (autonomous) "Floppa VPN (always-on)" else "Floppa VPN")
            .setContentText(state)
            .setSmallIcon(android.R.drawable.ic_lock_lock)
            .setOngoing(true)
            .setContentIntent(createOpenAppIntent())
            .build()
    }

    private fun startVpnForeground(connected: Boolean) {
        val notification = buildNotification(connected)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SYSTEM_EXEMPTED,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    /**
     * Promote the notification once a tunnel is actually up.
     *
     * Called from Rust after `start_tunnel` succeeds. Until then the service is foreground and
     * holding a descriptor, which is not the same thing as being connected — and the notification
     * used to claim it was from the moment the service started, including for the whole of a start
     * that went on to fail.
     */
    fun setConnected(connected: Boolean) {
        mainHandler.post {
            val manager = getSystemService(NotificationManager::class.java)
            manager.notify(NOTIFICATION_ID, buildNotification(connected))
        }
    }

    /** Create a PendingIntent that opens the app when the notification is tapped. */
    private fun createOpenAppIntent(): PendingIntent {
        val intent = packageManager.getLaunchIntentForPackage(packageName) ?: Intent()
        intent.flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED
        return PendingIntent.getActivity(
            this,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    /** Build and establish the TUN described by [spec], whichever source it came from. */
    private fun createTunInterface(spec: TunSpec): ParcelFileDescriptor {
        Log.i(
            TAG,
            "Creating TUN: ipv4=${spec.ipv4Addr}, ipv6=${spec.ipv6Addr}, mtu=${spec.mtu}, routes=${spec.routes.size}, dns=${spec.dns}",
        )

        val builder = Builder().setSession("Floppa VPN").setMtu(spec.mtu).setBlocking(false)

        // Add IPv4 address
        val (ipv4, prefix4) = parseAddress(spec.ipv4Addr)
        builder.addAddress(ipv4, prefix4)

        // Add IPv6 address if provided
        spec.ipv6Addr?.let {
            val (ipv6, prefix6) = parseAddress(it)
            builder.addAddress(ipv6, prefix6)
        }

        // Add routes
        for (route in spec.routes) {
            try {
                val (addr, prefix) = parseAddress(route)
                builder.addRoute(addr, prefix)
            } catch (e: Exception) {
                Log.w(TAG, "Invalid route: $route", e)
            }
        }

        // Add DNS servers (may be comma-separated, e.g. "1.1.1.1, 8.8.8.8")
        spec.dns?.let {
            val servers =
                it.split(",")
                    .map { server -> server.trim() }
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
        if (spec.allowedApps.isNotEmpty()) {
            var included = 0
            for (app in spec.allowedApps) {
                try {
                    builder.addAllowedApplication(app)
                    included++
                } catch (e: Exception) {
                    Log.w(TAG, "Cannot include app: $app", e)
                }
            }
            // A builder with no allowed application routes *every* app, which is the opposite
            // of what the user asked for. Refuse rather than silently widen the tunnel — this
            // happens when every selected app has since been uninstalled.
            if (included == 0) {
                throw IllegalStateException(
                    "none of the ${spec.allowedApps.size} apps selected for the tunnel are installed"
                )
            }
        } else {
            for (app in spec.disallowedApps) {
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
     * Protect a socket from VPN routing. Called from Rust JNI to ensure UDP sockets bypass the VPN,
     * preventing routing loops.
     */
    fun protectSocket(socket: Int): Boolean {
        return protect(socket)
    }
}
