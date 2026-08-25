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
import android.os.IBinder
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.core.app.NotificationCompat
import java.io.File
import org.json.JSONArray
import org.json.JSONObject

/**
 * What `VpnService.Builder` is given, as the actor derived it.
 *
 * It arrives as JSON from Rust, in this process, at the moment the ladder needs a descriptor —
 * never in an intent any more. The address, routes and MTU are required: a TUN built from a made-up
 * default would carry a default route into a tunnel nobody configured, so anything missing is an
 * error rather than a fallback.
 */
data class TunSpec(
    /**
     * Generation this descriptor belongs to; quoted back when it arrives and matched on teardown.
     *
     * Minted per request for a TUN, never reused — deliberately not the intent's epoch, which every
     * protocol and pass of one connect cycle shares.
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
        /** The JSON `startGeneration` is called with — `TunSpec` in `vpn/autostart.rs`. */
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
 * The `:vpn` process: the tunnel, and everything that decides about it.
 *
 * Runs in its own process (`android:process=":vpn"` in the manifest) and hosts the Rust actor — the
 * intent, the connect ladder, the reconnect budget, the config store. This class is what only
 * Android can provide: consent, a TUN descriptor, a foreground notification, and the network
 * callbacks. The UI process reaches the actor over a Unix socket and holds no tunnel state at all.
 *
 * Three ways it comes to exist, and they differ only in who asked:
 * - **bound** by the UI, which is what makes the process (and the actor, and the store) exist while
 *   the app is open. No notification: nothing is running yet.
 * - **started** by the UI before a connect, so the service outlives the app being swiped away.
 * - **started by the system** for always-on VPN, at boot, or to restore a lockdown session — with
 *   no UI process anywhere. Then [nativeSystemStart] raises the intent from what the last
 *   successful connect recorded, and the actor does the rest.
 */
class FloppaVpnService : VpnService() {

    companion object {
        private const val TAG = "FloppaVpnService"
        private const val NOTIFICATION_CHANNEL_ID = "vpn_service"
        private const val NOTIFICATION_ID = 1

        /** Stop the tunnel, from the UI process or from the notification. */
        const val ACTION_STOP = "dev.okhsunrog.floppavpn.STOP_VPN"

        /**
         * "Be started, not merely bound."
         *
         * Sent by the UI before it asks for a tunnel. A bound-only service dies with its last
         * client, and the whole point of the tunnel living here is that it survives the app going
         * away.
         */
        const val ACTION_KEEP_ALIVE = "dev.okhsunrog.floppavpn.KEEP_ALIVE"

        /**
         * "This instance is serving nothing". No generation is ever minted as this, so a teardown
         * that arrives after the one it belonged to has gone matches nothing.
         */
        private const val NO_GENERATION = 0L

        init {
            System.loadLibrary("floppa_client_lib")
        }
    }

    // Native methods implemented in Rust (vpn/jni_entry.rs).

    /**
     * Boot the process: logging, the config store, the tunnel actor and the socket the UI reaches
     * it through. Idempotent — later service instances only refresh the reference Rust calls back
     * on. Throws [RuntimeException] when the actor cannot be served, which is unrecoverable.
     */
    private external fun nativeInit(logDir: String, dataDir: String)

    /**
     * Hands the descriptor `establish()` produced to the generation that asked for it. Throws
     * [RuntimeException] when that generation has since been superseded.
     */
    private external fun nativeSetTunFd(generation: Long, tunFd: Int)

    /**
     * Records why a TUN could not be established, so the ladder gets a reason on its next look
     * instead of waiting out its budget.
     */
    private external fun nativeReportStartError(generation: Long, message: String)

    /**
     * The phone's default network changed under a running tunnel: rebind its socket in place. The
     * tunnel, its descriptor and its routes are unaffected — only the socket was bound to a network
     * that is now gone.
     */
    private external fun nativeNetworkChanged(generation: Long)

    /**
     * The system asked for a tunnel with nobody watching — always-on, boot, lockdown. Raises the
     * intent from what the last successful connect recorded, or stops the service when there is
     * nothing to raise.
     */
    private external fun nativeSystemStart()

    /** Ask the actor to go down. The tunnel, the notification and this service go with it. */
    private external fun nativeRequestStop()

    /** This service instance is being destroyed; end its generation and the tunnel on it. */
    private external fun nativeServiceGone(generation: Long)

    /**
     * Generation of the descriptor this instance is holding.
     *
     * Quoted back on teardown so a late `onDestroy` from an instance that has been replaced cannot
     * end the generation belonging to the one that replaced it — these instances share a process,
     * and stopping one is asynchronous.
     */
    private var generation: Long = NO_GENERATION

    /**
     * The most recent `startId`. `stopSelf(startId)` refuses to stop the service when a newer start
     * has arrived since, which a bare `stopSelf()` cannot see.
     */
    private var lastStartId: Int = 0

    private var tunInterface: ParcelFileDescriptor? = null

    /** Whether the foreground notification is up, so a stop with nothing running is a no-op. */
    private var foreground = false

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
        // Boots the actor on the first instance; refreshes the callback reference on every one.
        nativeInit(logDir.absolutePath, applicationInfo.dataDir)
        Log.i(TAG, "the VPN process is up")
    }

    /**
     * The UI binds this service to keep the process — and so the actor and the config store — alive
     * while the app is open. There is no Binder API: everything goes over the socket, and a null
     * binding holds the process just as well.
     *
     * The system's own VPN binding is a different action and still goes to `VpnService`.
     */
    override fun onBind(intent: Intent?): IBinder? =
        if (intent?.action == SERVICE_INTERFACE) super.onBind(intent) else null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Log.i(TAG, "onStartCommand: action=${intent?.action}, startId=$startId")
        lastStartId = startId

        when (intent?.action) {
            // The user asked to stop, from the app or from the notification. The actor decides
            // what that means for the tunnel; this service goes away when it says so.
            ACTION_STOP -> {
                Log.i(TAG, "stop requested")
                nativeRequestStop()
            }

            // The UI is about to ask for a tunnel and wants this service to outlive it. Foreground
            // immediately: `startForegroundService` gives us seconds, not minutes, and what the
            // notification says at this point is "connecting", which is true.
            ACTION_KEEP_ALIVE -> startVpnForeground(connected = false)

            // A start the system issued: always-on, boot, or a lockdown restore. Same requirement
            // — foreground at once — and then the actor is told to want a tunnel.
            else -> {
                if (intent == null) {
                    // START_NOT_STICKY means we should never be redelivered a null intent; some
                    // OEM builds do it anyway. Treated as a system start, but said out loud, so
                    // that a device doing it does not read as always-on in the log.
                    Log.w(TAG, "started with a null intent; treating it as a system start")
                }
                startVpnForeground(connected = false)
                nativeSystemStart()
            }
        }

        // Not sticky. A restart would arrive with a null intent and nothing to do; when the system
        // wants this service back it starts it itself, with the VpnService action.
        return START_NOT_STICKY
    }

    /**
     * Whether this app already holds VPN consent. Called from Rust.
     *
     * A question, never a dialog: `VpnService.prepare` can be checked from anywhere and can only be
     * *shown* from an activity, which this process does not have. Consent that is missing comes
     * back as a refusal, and the UI — which does have an activity — is what asks for it.
     */
    fun hasConsent(): Boolean = VpnService.prepare(this) == null

    /**
     * Establish a TUN for [generation] and hand its descriptor to the actor. Called from Rust.
     *
     * Asynchronous by nature: `establish()` must run on the main thread, so this posts and answers
     * by calling back — [nativeSetTunFd] on success, [nativeReportStartError] on failure. The
     * ladder waits by *observing*, which is what makes "still coming up", "failed, and here is why"
     * and "gone" three different answers instead of one timeout.
     */
    fun startGeneration(planJson: String, generation: Long) {
        mainHandler.post {
            // A descriptor left over from a previous generation goes now: the old tunnel has
            // already been stopped, and leaving its fd open leaks it.
            closeTun()

            val spec =
                try {
                    TunSpec.fromJson(planJson)
                } catch (e: Exception) {
                    Log.e(TAG, "the TUN spec does not describe a usable interface", e)
                    reportStartError(generation, e)
                    return@post
                }

            this.generation = generation
            startVpnForeground(connected = false)
            try {
                val tun = createTunInterface(spec)
                tunInterface = tun
                Log.i(TAG, "TUN established with fd ${tun.fd} for generation $generation")
                nativeSetTunFd(generation, tun.fd)
                watchNetwork(generation)
            } catch (e: Exception) {
                // Every reason `establish()` fails is outside this app — consent revoked, another
                // VPN holding lockdown, every selected app uninstalled — so the reason is worth
                // more than the failure.
                Log.e(TAG, "failed to establish the TUN", e)
                closeTun()
                reportStartError(generation, e)
            }
        }
    }

    private fun reportStartError(generation: Long, e: Exception) {
        try {
            nativeReportStartError(generation, e.message ?: e.toString())
        } catch (report: Exception) {
            Log.w(TAG, "could not record the start error", report)
        }
    }

    override fun onDestroy() {
        Log.i(TAG, "the VPN service is being destroyed")
        endGeneration()
        // The reference Rust calls back on is deliberately *not* cleared here. Instances share this
        // process and their teardown is asynchronous: a new instance's onCreate routinely runs
        // before the old one's onDestroy, so clearing here would clear the reference the new
        // instance had just installed — and `protectSocket` failing means the live tunnel's own
        // handshake traffic routes into itself. It is replaced on the next onCreate instead.
        super.onDestroy()
    }

    /**
     * The user revoked VPN consent, or another VPN took over.
     *
     * The descriptor is already dead by the time this runs. Ending the generation stops the tunnel
     * on it, and the actor sees a tunnel that is no longer running — what it does about that is its
     * decision, exactly as for a tunnel that died any other way.
     */
    override fun onRevoke() {
        Log.i(TAG, "VPN consent was revoked")
        endGeneration()
        super.onRevoke()
    }

    private fun endGeneration() {
        val target = generation
        generation = NO_GENERATION
        stopWatchingNetwork()
        closeTun()
        if (target != NO_GENERATION) {
            nativeServiceGone(target)
        }
    }

    /**
     * Drop the notification and stop, from Rust: the actor has nothing running any more.
     *
     * A no-op when this service is not foreground — the actor reports Disconnected as soon as it
     * starts, before anyone has asked for anything, and that must not stop a service the UI has
     * only bound.
     */
    fun shutdownService() {
        mainHandler.post {
            if (!foreground) return@post
            Log.i(TAG, "nothing is running; the service is standing down")
            stopWatchingNetwork()
            closeTun()
            stopForeground(STOP_FOREGROUND_REMOVE)
            foreground = false
            // By startId, so a start that arrived after this was scheduled keeps the service alive.
            stopSelf(lastStartId)
        }
    }

    private fun closeTun() {
        tunInterface?.let { tun ->
            Log.i(TAG, "closing the TUN interface")
            try {
                tun.close()
            } catch (e: Exception) {
                Log.w(TAG, "error closing the TUN interface", e)
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
            .setContentTitle("Floppa VPN")
            .setContentText(state)
            .setSmallIcon(android.R.drawable.ic_lock_lock)
            .setOngoing(true)
            .setContentIntent(createOpenAppIntent())
            .build()
    }

    /** Idempotent: `startForeground` on a service that already is one only replaces the notice. */
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
        foreground = true
    }

    /**
     * What the notification says, following the actor's own phase. Called from Rust.
     *
     * The notification is the only UI a tunnel has while the app is closed, so it is written from
     * the one place that knows what is true — not by whatever last touched the tunnel, which is how
     * it used to claim "connected" for the whole of a start that went on to fail.
     */
    fun setConnected(connected: Boolean) {
        mainHandler.post {
            if (!foreground) return@post
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
