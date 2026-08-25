package dev.okhsunrog.floppavpn.vpn

import android.app.PendingIntent
import android.content.Intent
import android.graphics.drawable.Icon
import android.net.VpnService
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import android.util.Log
import java.io.File

/**
 * The Quick Settings tile: connect and disconnect without opening the app.
 *
 * Runs in `:vpn` (`android:process=":vpn"` in the manifest), which is what makes it cheap and
 * honest at once. Cheap, because the phase it shows is a process-local read — no binder, no socket,
 * nothing to keep warm. Honest, because when there is no `:vpn` process there is no tunnel, and a
 * freshly created process reads [VpnPhase.Off], which is exactly right.
 *
 * A tap is not a request to the UI: there may be no UI. It is the same start the system issues for
 * always-on, and the actor raises the intent from what the last successful connect recorded. Two
 * things that start cannot do for itself are checked here first, because both have an answer that
 * is "open the app" rather than "try and fail":
 * - **consent**, which only an activity can ask for;
 * - **something to raise**, because a device that has never connected has no intent to repeat.
 */
class FloppaVpnTileService : TileService() {

    companion object {
        private const val TAG = "FloppaVpnTile"

        /**
         * The last-good intent, written by the actor.
         *
         * Named here because the tile has to know whether there is anything to start *before*
         * starting anything, and the file is the only evidence available without booting the actor.
         * Kept in step with `BUNDLE_FILENAME` in `vpn/autostart.rs`, which says so too.
         */
        private const val AUTOSTART_FILENAME = "autostart.json"
    }

    /** Wakes the tile when the service publishes a new phase. Held so it can be unregistered. */
    private val onPhaseChanged = Runnable { refresh() }

    override fun onStartListening() {
        super.onStartListening()
        VpnPhaseHolder.watch(onPhaseChanged)
        refresh()
    }

    override fun onStopListening() {
        VpnPhaseHolder.unwatch(onPhaseChanged)
        super.onStopListening()
    }

    override fun onClick() {
        super.onClick()
        when (VpnPhaseHolder.current()) {
            // Anything in motion or up: ask the actor to go down. What that means for the tunnel
            // is its decision, exactly as when the notification's Stop is used.
            VpnPhase.Connected,
            VpnPhase.Busy -> stopTunnel()
            VpnPhase.Off -> startTunnel()
        }
    }

    private fun stopTunnel() {
        // `startService`, not `startForegroundService`: this arm never raises a notification and
        // the service may stop within seconds of receiving it, which is precisely the shape the
        // foreground-start deadline punishes.
        val intent =
            Intent(this, FloppaVpnService::class.java).setAction(FloppaVpnService.ACTION_STOP)
        try {
            startService(intent)
        } catch (e: Exception) {
            Log.e(TAG, "could not ask the tunnel to stop", e)
        }
        // Optimistic, and corrected the moment the service publishes: a tile that does not react
        // to its own tap reads as broken.
        showPhase(VpnPhase.Busy)
    }

    private fun startTunnel() {
        if (VpnService.prepare(this) != null) {
            Log.i(TAG, "no VPN consent yet; sending the user to the app")
            openApp()
            return
        }
        if (!File(applicationInfo.dataDir, AUTOSTART_FILENAME).exists()) {
            Log.i(TAG, "nothing has ever connected on this device; sending the user to the app")
            openApp()
            return
        }
        val intent =
            Intent(this, FloppaVpnService::class.java).setAction(FloppaVpnService.ACTION_TILE_START)
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                startForegroundService(intent)
            } else {
                startService(intent)
            }
            showPhase(VpnPhase.Busy)
        } catch (e: Exception) {
            Log.e(TAG, "could not start the tunnel from the tile", e)
            refresh()
        }
    }

    /**
     * Send the user to the app, unlocking first if the phone is locked.
     *
     * Tiles are tappable on the lock screen. Starting a tunnel there is fine — the actor reads
     * files that are available after the first unlock, which is the same ground always-on stands on
     * — but showing an activity is not, so that branch waits for the unlock.
     */
    private fun openApp() {
        val launch =
            packageManager.getLaunchIntentForPackage(packageName)?.apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED
            }
        if (launch == null) {
            Log.e(TAG, "the app has no launch intent")
            return
        }
        if (isLocked) {
            unlockAndRun { collapseInto(launch) }
        } else {
            collapseInto(launch)
        }
    }

    private fun collapseInto(launch: Intent) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            val pending =
                PendingIntent.getActivity(
                    this,
                    0,
                    launch,
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
            startActivityAndCollapse(pending)
        } else {
            // The `PendingIntent` overload is API 34 and throws on anything below it, so the
            // deprecated one is not a leftover — it is the only call that exists down here.
            @Suppress("DEPRECATION", "StartActivityAndCollapseDeprecated")
            startActivityAndCollapse(launch)
        }
    }

    private fun refresh() = showPhase(VpnPhaseHolder.current())

    private fun showPhase(phase: VpnPhase) {
        val tile = qsTile ?: return
        tile.state = if (phase == VpnPhase.Off) Tile.STATE_INACTIVE else Tile.STATE_ACTIVE
        tile.icon = Icon.createWithResource(this, R.drawable.ic_qs_tile_vpn)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            tile.subtitle =
                when (phase) {
                    VpnPhase.Off -> "Disconnected"
                    VpnPhase.Busy -> "Connecting…"
                    VpnPhase.Connected -> "Connected"
                }
        }
        tile.updateTile()
    }
}
