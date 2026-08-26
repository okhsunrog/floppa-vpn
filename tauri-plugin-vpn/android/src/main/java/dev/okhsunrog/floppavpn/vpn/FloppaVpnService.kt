package dev.okhsunrog.floppavpn.vpn

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
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
         * The Quick Settings tile was tapped on.
         *
         * Handled exactly as a system start — there is no UI process to ask, so the intent comes
         * from what the last successful connect recorded — but named, so a tile tap is not logged
         * as the null-intent OEM oddity the system-start arm warns about.
         */
        const val ACTION_TILE_START = "dev.okhsunrog.floppavpn.TILE_START"

        /**
         * "This instance is serving nothing". No generation is ever minted as this, so a teardown
         * that arrives after the one it belonged to has gone matches nothing.
         */
        private const val NO_GENERATION = 0L

        /**
         * How long a service that was started may wait for the actor to actually be given something
         * to do. Generous next to an RPC round trip, short next to anything a person would notice.
         */
        private const val WORK_DEADLINE_MS = 10_000L

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
     * The device gained or lost its network entirely — not a roam, the presence or absence of one.
     *
     * Process-wide and generation-free, because it is a fact about the phone rather than about any
     * tunnel: it is reported when there is no tunnel at all, which is when the actor most needs it.
     * A parked cycle resumes on the `true`, and stops burning its budget on the `false`.
     */
    private external fun nativeLinkChanged(online: Boolean)

    /**
     * Nobody is watching the network any more, so the last thing this class said about it should no
     * longer be believed.
     *
     * Its own entry point rather than a third value on [nativeLinkChanged], because it is a
     * different kind of statement: that one reports what is observed, this one says observation has
     * stopped. The actor's `Link::Unknown` gates nothing, which is exactly right for a fact that
     * has no one left to keep it true.
     */
    private external fun nativeLinkUnwatched()

    /**
     * How the system is running this VPN: whether we are its always-on VPN, and whether lockdown is
     * on with it.
     *
     * All three in one call because they are one answer — `isLockdownEnabled` is documented as
     * *"running in always-on VPN lockdown mode"*, a mode of always-on — and separate calls could be
     * seen half-applied. `known` is what keeps "we could not ask" from arriving as a definite "no";
     * Rust turns the triple into one value.
     *
     * Not "who started this tunnel". Both queries ask the system whether *this app* is configured
     * as the always-on VPN, which is true just as much when a person presses Connect in the app.
     */
    private external fun nativeVpnModeChanged(known: Boolean, alwaysOn: Boolean, lockdown: Boolean)

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

    /** Watches the network under the tunnel — and, just as importantly, under no tunnel at all. */
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    /** The network the tunnel is currently riding, so only a real change bounces the socket. */
    private var underlyingNetwork: Network? = null

    /**
     * Every non-VPN network this callback has been told about and not told to forget.
     *
     * A set rather than a flag because below API 31 "lost a network" and "lost the network" are
     * different events: the plain registration reports every match, and a phone dropping Wi-Fi with
     * mobile data already up has lost nothing worth telling the actor about. On API 31+ the
     * best-matching registration reports one network at a time and is maintained by replacement —
     * see [onNetworkAvailable], where treating the two modes alike was a real bug.
     */
    private val availableNetworks = mutableSetOf<Network>()

    /**
     * Whether the watch reports only the single best network, rather than every matching one.
     *
     * The difference decides how [availableNetworks] is maintained, so it is named once here
     * instead of being re-derived from the SDK level at each of the places that care.
     */
    private val bestMatchOnly = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S

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
        try {
            // Boots the actor on the first instance; refreshes the callback reference on every one.
            nativeInit(logDir.absolutePath, applicationInfo.dataDir)
            Log.i(TAG, "the VPN process is up")
            // After the actor exists, so its first report has somewhere to land.
            watchNetwork()
        } catch (e: Exception) {
            // Caught rather than thrown on: an exception out of onCreate takes the process with it,
            // and the UI binds on every launch — so a boot that cannot succeed would be a crash
            // loop instead of an app that says it cannot reach the tunnel. The socket will not
            // exist, which is exactly what the UI renders as "not reachable".
            Log.e(TAG, "the VPN process could not boot", e)
        }
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
        // Cheap, and the one moment a Settings change can reach us: toggling always-on or lockdown
        // reconfigures the VPN, which arrives as a start. It answers "unknown" when no tunnel is
        // established yet, which is the truth at that point.
        reportVpnMode()

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
            //
            // "About to" is a promise, and this is what happens when it is not kept: the request
            // that follows can be refused — no usable config, a wipe in between — and a refusal
            // changes no phase, so nothing would ever stand this service back down. It gets a
            // deadline instead, cancelled the moment the actor is visibly working.
            ACTION_KEEP_ALIVE -> {
                val phase = VpnPhaseHolder.current()
                startVpnForeground(connected = phase == VpnPhase.Connected)
                // Only wait for work when none is under way. The UI starts this service and asks
                // for a tunnel over the socket, and the two race: a request that lands first can
                // leave the actor with nothing further to say, and a deadline armed against that
                // would stand a live tunnel down ten seconds later. The phase this process last
                // heard is the cheapest honest answer to "is anything happening".
                if (phase == VpnPhase.Off) awaitWork()
            }

            // A start the system issued — always-on, boot, a lockdown restore — or the tile, which
            // has no more context than the system does. Same requirement, foreground at once, and
            // then the actor is told to want a tunnel.
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
                // The watch is already running; what is new is a tunnel to attribute to it.
                underlyingNetwork?.let { setUnderlyingNetworks(arrayOf(it)) }
                // Now, and not before: establish() is what makes us the VPN's owner, which is what
                // the always-on queries require before they will answer at all.
                reportVpnMode()
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
        VpnPhaseHolder.publish(VpnPhase.Off)
        stopWatchingNetwork()
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
        // The network watch deliberately outlives the tunnel: a parked cycle is waiting on exactly
        // the report it produces. It goes with the service instance, in onDestroy.
        closeTun()
        // The VPN mode does not outlive it, because it cannot be asked without one. Retracted to
        // "unknown" rather than left standing, for the same reason the network watch retracts.
        reportVpnMode()
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
            // Ahead of the foreground check, for the same reason [setState] publishes early: a
            // bound-only service settling at Disconnected still has to put the tile back to Off.
            VpnPhaseHolder.publish(VpnPhase.Off)
            if (!foreground) return@post
            Log.i(TAG, "nothing is running; the service is standing down")
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
     * Follow the network under the tunnel, for as long as this service instance exists.
     *
     * Registered once, in `onCreate`, rather than once per tunnel — and that is the change that
     * makes the actor able to wait out a network outage. It has to answer "is there a network" when
     * there is *no* tunnel, because that is precisely the state a phone in a tunnel is in: the
     * tunnel died, the cycle is parked, and the only thing that can restart it is the network
     * coming back. A watch that lived and died with the tunnel could never report that.
     *
     * One callback, three jobs, and only the first of them is unconditional:
     *
     * - **The link.** Report to the actor whether this device has any usable network at all. It
     *   parks its cycles on `Offline` and resumes them on `Online`, spending no budget in between.
     * - **`setUnderlyingNetworks`.** Tell the system what the VPN is carried by, so accounting and
     *   "is there a network" are right for every app inside the tunnel.
     * - **The rebind.** A *change* of that network is the most common way a mobile tunnel breaks:
     *   the socket underneath stays bound to a network that no longer exists, so every packet falls
     *   into a hole while the tunnel still looks perfectly up. A round trip fixes it, with no UI
     *   process and no change to what is running — so it can never fight the actor's own recovery,
     *   which starts a whole cycle and takes minutes to reach.
     *
     * The last two only mean anything while a generation owns a tunnel, and keep their guard.
     */
    private fun watchNetwork() {
        stopWatchingNetwork()
        val manager = getSystemService(ConnectivityManager::class.java)
        if (manager == null) {
            Log.w(TAG, "No ConnectivityManager: the tunnel will not follow network changes")
            return
        }
        val callback =
            object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    mainHandler.post { onNetworkAvailable(network) }
                }

                override fun onLost(network: Network) {
                    mainHandler.post { onNetworkLost(network) }
                }
            }
        // Explicitly *not* the default network. Once this service is up, the default network is
        // our own tunnel — so a default-network callback reports the VPN and then goes silent
        // through the very events it exists to catch: on this device a Wi-Fi to mobile switch
        // produced no callback at all. What is wanted is the best network that is not a VPN,
        // which is what the tunnel is actually carried by.
        val request =
            NetworkRequest.Builder()
                .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                .build()
        try {
            if (bestMatchOnly) {
                // "The one network that would be the default if we were not here" — exactly the
                // question, answered by the system rather than guessed at from a list.
                manager.registerBestMatchingNetworkCallback(request, callback, mainHandler)
            } else {
                // Older devices get every matching network and the most recent one wins. Coarser,
                // and the rebind it triggers is idempotent, so being wrong costs a handshake. The
                // callback arrives on a system thread there; it hops to the main one itself.
                manager.registerNetworkCallback(request, callback)
            }
            networkCallback = callback
        } catch (e: Exception) {
            Log.w(TAG, "Could not watch the underlying network", e)
            return
        }
        // Registering does not, by itself, tell us there is nothing: with no matching network the
        // callback simply never fires, and staying silent would leave the actor at `Unknown` —
        // which gates nothing, so a boot in airplane mode would spend its budget before the first
        // report arrived. A device with no default network at all has no network at all, VPN or
        // otherwise, and that is a fact worth reporting immediately. Anything else is left to the
        // callback, which is the only thing that can tell a real network from our own tunnel.
        if (manager.activeNetwork == null) {
            reportLink(online = false)
        }
    }

    private fun onNetworkAvailable(network: Network) {
        val wasEmpty = availableNetworks.isEmpty()
        // On the best-matching registration an `onAvailable` *replaces* what was there. It reports
        // one network — the best — and sends no `onLost` for one that has merely stopped being
        // best, so a network that loses to a better one is never taken back out. Accumulating
        // them is not a leak, it is a wrong answer: Wi-Fi returning alongside mobile left the set
        // holding both, and when every network then went away the set still held the mobile one
        // it had never been told about. Nothing reported the outage, the actor judged the peer's
        // silence as it always had, and the tunnel died exactly as before this gate existed.
        //
        // Below API 31 the plain registration does report every matching network, and every loss
        // of one, so there the set is the right shape and is kept.
        if (bestMatchOnly) availableNetworks.clear()
        availableNetworks.add(network)
        if (wasEmpty) reportLink(online = true)

        // Recorded whether or not a tunnel exists, and that ordering is load-bearing now that the
        // watch outlives the tunnel. The first onAvailable arrives at registration, long before
        // anyone connects; skipping the write then would leave this null for the tunnel that
        // follows — so `startGeneration` would never call `setUnderlyingNetworks`, and the first
        // roam after it would look like a first sighting and skip the rebind, leaving the socket
        // pinned to a network that no longer exists. The per-generation registration used to hide
        // this by re-firing onAvailable after every establish.
        val previous = underlyingNetwork
        underlyingNetwork = network

        // Below this line is about the tunnel, and there may not be one.
        if (generation == NO_GENERATION) return
        setUnderlyingNetworks(arrayOf(network))
        if (previous == null) {
            Log.i(TAG, "Tunnel is riding $network")
            return
        }
        if (previous == network) return

        Log.i(TAG, "The tunnel's network changed: $previous -> $network; rebinding")
        try {
            nativeNetworkChanged(generation)
        } catch (e: Exception) {
            Log.w(TAG, "Could not tell the tunnel its network changed", e)
        }
    }

    /**
     * A network went away.
     *
     * Only the *last* one is news: on API 31+ the best-matching registration means there is only
     * ever one, and below it a phone dropping Wi-Fi while mobile data carries on has lost nothing
     * that matters.
     *
     * [underlyingNetwork] is deliberately not cleared. It is the memory of what the tunnel's socket
     * is bound to, and it is still bound to it — a dead network. Forgetting it here would make the
     * next `onAvailable` look like the first one and skip the rebind, leaving the socket pinned to
     * a network that no longer exists: the exact failure the rebind exists for.
     */
    private fun onNetworkLost(network: Network) {
        availableNetworks.remove(network)
        if (availableNetworks.isEmpty()) {
            Log.i(TAG, "there is no network under the tunnel any more")
            reportLink(online = false)
        }
    }

    /** Tell the actor what the device's network situation is. Never fatal. */
    private fun reportLink(online: Boolean) {
        try {
            nativeLinkChanged(online)
        } catch (e: Exception) {
            Log.w(TAG, "Could not report the network's state", e)
        }
    }

    /**
     * Ask the system how it is running this VPN, and tell the actor.
     *
     * Silent below API 29, where neither query exists: the actor's `Unknown` then stands, which is
     * the honest answer — an older device can perfectly well *be* the always-on VPN, so reporting
     * "no" would be a lie rather than a default. A call that throws is left the same way.
     */
    private fun reportVpnMode() {
        // Only answerable while our VPN is actually established. `isAlwaysOn()` reaches
        // `isCallerCurrentAlwaysOnVpnApp()`, which is `getVpnIfOwner() != null &&
        // vpn.getAlwaysOn()`
        // — and there is no owner until `establish()` has run. Asking before that gets `false` from
        // both, and that `false` means "you are not the owner", not "always-on is off". Publishing
        // it as a definite no is exactly the mistake `Link` exists to prevent, and it shipped once:
        // a service the system itself started for always-on reported `Off` from `onCreate`.
        //
        // The live descriptor is the test, and `generation` is not: a generation is set from the
        // moment one is *requested*, which is before `establish()` and still true for a moment
        // after a teardown. Gating on it produced a real `Off` on the device — 130 ms of it,
        // between a disconnect and the reconnect that followed.
        //
        // Below API 29 neither query exists, which is the same "cannot ask" and takes the same
        // path. So does a call that throws.
        val canAsk = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && tunInterface != null
        val mode =
            if (canAsk) {
                try {
                    Triple(true, isAlwaysOn, isLockdownEnabled)
                } catch (e: Exception) {
                    Log.w(TAG, "Could not ask the system how it is running us", e)
                    Triple(false, false, false)
                }
            } else {
                Triple(false, false, false)
            }
        try {
            nativeVpnModeChanged(mode.first, mode.second, mode.third)
        } catch (e: Exception) {
            Log.w(TAG, "Could not report the system's VPN mode", e)
        }
    }

    /** Tell the actor to stop believing the last report. Never fatal. */
    private fun reportUnwatched() {
        try {
            nativeLinkUnwatched()
        } catch (e: Exception) {
            Log.w(TAG, "Could not withdraw the network report", e)
        }
    }

    private fun stopWatchingNetwork() {
        val callback = networkCallback ?: return
        networkCallback = null
        underlyingNetwork = null
        availableNetworks.clear()
        // Retract the verdict along with the watch. `Offline` is a live report, and a live report
        // with nobody left to update it is the worst of both: the actor would park the next
        // connect for ever on the last thing a watcher said before it stopped watching. `Unknown`
        // is what "nobody is looking" means, and it gates nothing.
        reportUnwatched()
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
     * What the actor's phase is, as often as it changes. Called from Rust.
     *
     * Two things follow from it. The notification is the only UI a tunnel has while the app is
     * closed, so what it says is written from the one place that knows what is true — not by
     * whatever last touched the tunnel, which is how it used to claim "connected" for the whole of
     * a start that went on to fail. And any phase that is not "we have not looked yet" cancels the
     * deadline a bare start armed: from here on the service stands down when the actor says so, not
     * when a timer runs out.
     *
     * `busy || connected` rather than `busy`, and the difference is a tunnel torn down ten seconds
     * after it came up. The UI starts this service and asks for a tunnel over the socket, and the
     * two race: the request can be accepted and the tunnel *connected* before the start intent is
     * delivered here. The deadline is then armed against an actor that has already finished its
     * work, and the only state left to arrive is Connected — which is not busy. `(false, false)` is
     * the one thing that does not count, because it means the actor has not observed anything yet
     * and so has not been given anything to do.
     */
    fun setState(busy: Boolean, connected: Boolean) {
        mainHandler.post {
            if (busy || connected) cancelAwaitWork()
            // Published before the foreground check: the tile follows the tunnel, not the
            // notification, and the actor can be working in a service the UI has only bound.
            VpnPhaseHolder.publish(if (connected) VpnPhase.Connected else VpnPhase.Busy)
            if (!foreground) return@post
            val manager = getSystemService(NotificationManager::class.java)
            manager.notify(NOTIFICATION_ID, buildNotification(connected))
        }
    }

    /**
     * Stand down if nothing comes of the start that armed this.
     *
     * Bounded by how long the UI takes to place its request, not by how long a connect takes: the
     * deadline is cancelled by the actor being *busy*, which happens within milliseconds of the
     * request landing, and a connect that then runs for a minute is never touched by it.
     */
    private fun awaitWork() {
        cancelAwaitWork()
        mainHandler.postDelayed(standDown, WORK_DEADLINE_MS)
    }

    private fun cancelAwaitWork() {
        mainHandler.removeCallbacks(standDown)
    }

    private val standDown = Runnable {
        Log.w(TAG, "started, but nothing asked for a tunnel; standing down")
        shutdownService()
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
