package dev.okhsunrog.floppavpn.vpn

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
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
    /**
     * Generation this instance serves; echoed over the RPC and matched on teardown.
     *
     * Minted per service start by the UI process (or, for an autonomous start, by Rust from the
     * reserved range) and never reused — deliberately not the intent's epoch, which every protocol
     * and pass of one connect cycle shares and which restarts at 1 in every UI process.
     */
    val generation: Long,
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
                generation = intent.getLongExtra(FloppaVpnService.EXTRA_GENERATION, 0L),
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
         * names the intent extras use (`TunSpec::with_generation` in `vpn/autostart.rs`).
         */
        fun fromJson(json: String): TunSpec {
            val o = JSONObject(json)
            return TunSpec(
                generation = o.getLong("generation"),
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
        const val EXTRA_GENERATION = "generation"

        /**
         * How long a generation whose TUN could not be established keeps answering the RPC, so the
         * UI (polling every 200 ms) reads `start_error` before the socket goes away.
         */
        private const val START_ERROR_LINGER_MS = 3_000L

        /**
         * "This instance is serving nothing". No start ever mints it, so a teardown that arrives
         * after the generation it belonged to has gone matches nothing.
         */
        private const val NO_GENERATION = 0L

        init {
            System.loadLibrary("floppa_client_lib")
        }
    }

    // Native methods implemented in Rust (vpn/jni_entry.rs)
    private external fun nativeInit(logDir: String)

    /** Binds the RPC socket for [generation]. Throws [RuntimeException] when it cannot. */
    private external fun nativeStartServer(socketPath: String, generation: Long)

    /**
     * Hands the descriptor `establish()` produced to the generation [nativeStartServer] bound.
     * Throws [RuntimeException] when [generation] is no longer the one serving.
     */
    private external fun nativeSetTunFd(generation: Long, tunFd: Int)

    /**
     * Records why this generation could not establish its TUN, so the UI process reads the reason
     * on its next poll instead of waiting for a service that never becomes ready.
     */
    private external fun nativeReportStartError(generation: Long, message: String)

    /**
     * For a start the system issued: the TUN to build from the autostart bundle, as the JSON
     * [TunSpec.fromJson] reads, with a fresh generation — or null when there is nothing to restore.
     */
    private external fun nativeLoadAutostart(dataDir: String): String?

    /**
     * After [nativeStartServer] on an autonomous start: bring the tunnel up from what
     * [nativeLoadAutostart] prepared. Throws [RuntimeException] when there is nothing to start
     * from; a start that fails later stops the service from the Rust side.
     */
    private external fun nativeStartTunnelFromBundle(generation: Long)

    /**
     * Generation this instance is currently serving.
     *
     * Passed back on teardown so a late onDestroy from a previous instance cannot stop the server
     * belonging to the one that replaced it — these instances share a process, and stopService is
     * asynchronous. [NO_GENERATION] once nothing is being served, so a teardown that arrives after
     * one matches nothing.
     */
    private var generation: Long = NO_GENERATION

    /**
     * The most recent `startId`. `stopSelf(startId)` refuses to stop the service when a newer start
     * has arrived since, which a bare `stopSelf()` cannot see — and a linger timer or a late
     * teardown from a superseded generation used to stop the instance that replaced it.
     */
    private var lastStartId: Int = 0

    /**
     * The phone's default network changed under a running tunnel: rebind its socket in place. The
     * tunnel, its descriptor and its routes are unaffected — only the socket was bound to a network
     * that is now gone.
     */
    private external fun nativeNetworkChanged(generation: Long)

    private external fun nativeStop(generation: Long)

    private var tunInterface: ParcelFileDescriptor? = null

    /** Whether this generation was started by the system rather than by the plugin. */
    private var autonomous = false

    /**
     * Watches which network the tunnel is riding on, for as long as one is up.
     *
     * Two things depend on it. `setUnderlyingNetworks` tells the system what the VPN is actually
     * carried by, which is what makes traffic accounting and "is there a network" correct for every
     * app inside the tunnel. And a *change* of that network is the single most common way a mobile
     * tunnel breaks: the socket underneath stays bound to a network that no longer exists, so every
     * packet falls into a hole while the tunnel still looks perfectly up. Rebinding it here takes a
     * round trip, needs no UI process, and never changes what is running — so it can never fight
     * the actor's own recovery, which starts a whole cycle and takes minutes to reach.
     */
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    /** The network the tunnel is currently riding, so only a real change bounces the socket. */
    private var underlyingNetwork: Network? = null

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
        Log.i(TAG, "onStartCommand: action=${intent?.action}, startId=$startId")
        lastStartId = startId

        if (intent == null) {
            Log.w(TAG, "Null intent, stopping service")
            stopSelf()
            return START_NOT_STICKY
        }

        // Handle stop request from UI process
        if (intent.action == ACTION_STOP) {
            Log.i(TAG, "Received STOP action, shutting down")
            closeGeneration(generation)
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
        // would run onDestroy, whose nativeStop(generation) matches this generation and tears it
        // down. Only an instance with nothing to keep alive acts on the intent.
        if (intent.action == SERVICE_INTERFACE || !intent.hasExtra(EXTRA_GENERATION)) {
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
            nativeStop(generation)
            generation = NO_GENERATION
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
     * process, on the same path the RPC start uses, under a generation from a range no UI process
     * can mint. A UI process that opens later finds it over the RPC — with its protocol and split
     * rules reported by this side — and adopts it.
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
        Log.i(TAG, "System start: rebuilding the last-good tunnel (generation=${spec.generation})")
        return startGeneration(spec, autonomous = true) {
            nativeStartTunnelFromBundle(spec.generation)
        }
    }

    /**
     * Go foreground, bind the RPC, establish the TUN, then run [afterReady] — the one step that
     * differs between a plugin start (nothing: the tunnel is requested over the socket) and an
     * autonomous one (start the tunnel from the bundle).
     *
     * The bind comes *before* `establish()` on purpose. `establish()` fails for reasons outside
     * this app — the user revoked the VPN consent, another VPN holds a lockdown, every app selected
     * for the tunnel was uninstalled — and with the socket already bound that failure is reported
     * over it: the UI's next poll gets the reason instead of waiting out a service that never comes
     * up. A failed bind, on the other hand, leaves nothing to report through, so the service simply
     * stops.
     */
    private fun startGeneration(spec: TunSpec, autonomous: Boolean, afterReady: () -> Unit): Int {
        // Before anything that can fail or tear down: onDestroy stops by this value, and reading
        // it later meant a start that threw left the previous instance's generation in the field.
        val generation = spec.generation
        this.generation = generation
        this.autonomous = autonomous

        // Foreground before the TUN exists, because Android requires it — so the notification says
        // what is true at this point, not what we hope for. `setConnected` promotes it once the
        // tunnel is actually carrying traffic.
        startVpnForeground(connected = false)

        try {
            // Keep in sync with SOCKET_NAME in rpc.rs.
            nativeStartServer(applicationInfo.dataDir + "/vpn.sock", generation)
        } catch (e: Exception) {
            // Nothing is listening, so nothing can be told; the service must not stay foreground
            // as if it had started.
            Log.e(TAG, "Failed to bind the RPC socket", e)
            closeGeneration(generation)
            return START_NOT_STICKY
        }

        try {
            tunInterface = createTunInterface(spec)
            val fd = tunInterface?.fd ?: throw IllegalStateException("Failed to get TUN fd")
            Log.i(TAG, "TUN interface created with fd: $fd")
            nativeSetTunFd(generation, fd)
            watchNetwork(generation)
            afterReady()
        } catch (e: Exception) {
            // The socket is bound, so this is reportable. Leave the generation answering for a
            // moment so the UI's next poll reads the reason, then wind it down — the UI's own
            // teardown usually gets there first, and closeGeneration is idempotent.
            Log.e(TAG, "Failed to establish the TUN", e)
            try {
                nativeReportStartError(generation, e.message ?: e.toString())
            } catch (report: Exception) {
                Log.w(TAG, "Could not record the start error", report)
            }
            // The generation is captured in a local: reading the field when the timer fires meant
            // the guard in closeGeneration compared a value against itself and always passed, so
            // this timer tore down whichever generation happened to be serving three seconds
            // later — usually the one that replaced this failed start.
            mainHandler.postDelayed({ closeGeneration(generation) }, START_ERROR_LINGER_MS)
            return START_NOT_STICKY
        }

        // Not sticky: a restart arrives with a null intent, and the null branch above can only
        // stop again. For always-on VPN it is the system that restarts the service, with the
        // VpnService action, and that start rebuilds from the bundle.
        return START_NOT_STICKY
    }

    /**
     * Tear down [target] and stop, unless a newer start has since taken over this instance — then
     * that generation is already gone and the newer one must be left alone. Idempotent: the field
     * is cleared, so a second call for the same generation finds nothing to do.
     */
    private fun closeGeneration(target: Long) {
        if (target == NO_GENERATION || generation != target) {
            Log.i(TAG, "closeGeneration($target): superseded by $generation, nothing to do")
            return
        }
        nativeStop(target)
        generation = NO_GENERATION
        cleanupAndroid()
        // By startId, so a start that arrived after this teardown was scheduled keeps the service
        // alive; a bare stopSelf() stopped it regardless of what had happened since.
        stopSelf(lastStartId)
    }

    override fun onDestroy() {
        Log.i(TAG, "VPN service destroying")
        // onDestroy is called by Android when the service is being torn down
        // (e.g., system kill). Stop Rust side and clean up.
        nativeStop(generation)
        generation = NO_GENERATION
        cleanupAndroid()
        super.onDestroy()
    }

    override fun onRevoke() {
        Log.i(TAG, "VPN permission revoked")
        nativeStop(generation)
        generation = NO_GENERATION
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
            generation = NO_GENERATION
            cleanupAndroid()
            stopSelf(lastStartId)
        }
    }

    private fun cleanupAndroid() {
        stopWatchingNetwork()
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

    /**
     * Follow the default network for as long as [generation] owns the tunnel.
     *
     * The first `onAvailable` after registering describes the network the tunnel was just built on,
     * so it is recorded and not acted on; only a *different* network from then on is a roam. The
     * callback is registered once per generation and removed with the rest of the Android-side
     * teardown.
     */
    private fun watchNetwork(generation: Long) {
        stopWatchingNetwork()
        val manager = getSystemService(ConnectivityManager::class.java)
        if (manager == null) {
            Log.w(TAG, "No ConnectivityManager: the tunnel will not follow network changes")
            return
        }
        val callback =
            object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    mainHandler.post { onDefaultNetwork(generation, network) }
                }

                override fun onLost(network: Network) {
                    // Not acted on: there is nothing to rebind onto until another network
                    // arrives, and that arrival is an onAvailable.
                    Log.i(TAG, "Lost network $network")
                }
            }
        try {
            manager.registerDefaultNetworkCallback(callback)
            networkCallback = callback
        } catch (e: Exception) {
            Log.w(TAG, "Could not watch the default network", e)
        }
    }

    private fun onDefaultNetwork(generation: Long, network: Network) {
        // A callback outliving its generation belongs to a tunnel that is already gone.
        if (this.generation != generation) return

        setUnderlyingNetworks(arrayOf(network))
        val previous = underlyingNetwork
        underlyingNetwork = network
        if (previous == null) {
            Log.i(TAG, "Tunnel is riding $network")
            return
        }
        if (previous == network) return

        Log.i(TAG, "Default network changed: $previous -> $network; rebinding the tunnel")
        try {
            nativeNetworkChanged(generation)
        } catch (e: Exception) {
            Log.w(TAG, "Could not tell the tunnel its network changed", e)
        }
    }

    private fun stopWatchingNetwork() {
        val callback = networkCallback ?: return
        networkCallback = null
        underlyingNetwork = null
        try {
            getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(callback)
        } catch (e: Exception) {
            Log.w(TAG, "Could not stop watching the default network", e)
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
