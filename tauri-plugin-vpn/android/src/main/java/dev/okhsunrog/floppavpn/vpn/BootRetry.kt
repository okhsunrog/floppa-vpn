package dev.okhsunrog.floppavpn.vpn

import android.app.ActivityManager
import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.provider.Settings
import android.util.Log
import java.io.File

/**
 * The file `vpn/autostart.rs` writes after every successful connect and removes on a wipe. Its
 * existence is the cheapest possible answer to "has anything ever connected on this device", which
 * is all the tile and the boot retry need from it.
 */
internal const val AUTOSTART_FILENAME = "autostart.json"

/**
 * A second chance for the always-on start, on devices that kill it.
 *
 * Android starts this app's `FloppaVpnService` at boot when it is the always-on VPN, and does so
 * exactly once: `Vpn.java` issues the start on user unlock and never retries, and the restart
 * `ActivityManager` schedules for the dying process is cancelled the moment the system's own
 * binding to it goes. On stock Android that is enough. On an Onyx Boox it is not: the launcher
 * applies its e-ink "App Optimization" (EAC) status about two and a half seconds after
 * `BOOT_COMPLETED`, and applying it *kills every running process* of every app the feature is
 * enabled for — which was the tunnel, already `Up`, half a second after it came up. Three boots,
 * three identical kills, and nothing left to bring it back.
 *
 * So the app asks for a second chance itself: a `BOOT_COMPLETED` receiver schedules a job some
 * twenty seconds out — held by the system, so it does not matter what happens to this process in
 * between — and the job starts the service as the system would have, only if nothing is running by
 * then. Twenty seconds is far past any launcher's initialisation and still well before a person
 * would go looking for the tunnel.
 *
 * This is gated on what the system itself did, not on what the app might like:
 *
 * - the system must have started the service *this boot*. That is the always-on question asked the
 *   only way an app may ask it — `Settings.Secure.always_on_vpn_app` is `@hide`, and from Android
 *   12 reading a hidden key throws; `VpnService.isAlwaysOn()` answers only once a tunnel is
 *   established. So the service leaves a marker when a genuine system start reaches it (the
 *   `android.net.VpnService` action, or the null intent some OEM builds deliver), stamped with the
 *   boot it happened in, and the receiver looks for a marker from the current boot. With always-on
 *   off Android starts nothing at boot, no marker is written, and neither does this — "the app
 *   never connects on its own unless the system asked" is the rule the whole system-start path
 *   rests on;
 * - consent must be held, because a background start cannot ask for it;
 * - something must have connected before, or there is no intent to raise.
 *
 * Lives in `:vpn`, beside the service and the tile, for the same reason the tile does: the phase it
 * reads is a process-local truth. A `:vpn` process that exists and says `Connected` needs nothing;
 * a `:vpn` process the job had to create says `Off`, and that is the truth too. Living there also
 * means nothing here may throw: an exception out of a receiver kills its process, and this process
 * may be carrying a tunnel that survived — the very thing this exists to protect.
 */
internal object BootRetry {
    private const val TAG = "FloppaVpnBoot"

    /** One job at a time; scheduling again replaces the pending one. */
    private const val JOB_ID = 0x0b007

    /**
     * Where a genuine system start records the boot it happened in. Holds one number, the
     * `BOOT_COUNT` at the time; a boot is identified by that rather than by a clock, because the
     * wall clock is still being set at the moment this matters.
     */
    private const val SYSTEM_START_MARKER = "system-start.boot"

    /**
     * How long after `BOOT_COMPLETED` the retry runs. Late enough for any launcher to have finished
     * whatever it does to processes at boot; a deadline keeps the scheduler from deferring it into
     * the next hour on a device that treats a fresh boot as "idle".
     */
    private const val RETRY_LATENCY_MS = 20_000L
    private const val RETRY_DEADLINE_MS = 60_000L

    /**
     * The boot this process is running in. `BOOT_COUNT` is public, readable by any app, and
     * incremented by `SystemServer` before the first app runs, so the system start and the retry
     * both see the same value.
     */
    private fun bootCount(context: Context): Int =
        Settings.Global.getInt(context.contentResolver, Settings.Global.BOOT_COUNT, -1)

    private fun marker(context: Context) =
        File(context.applicationInfo.dataDir, SYSTEM_START_MARKER)

    /** A genuine system start reached the service: remember which boot that was. */
    fun recordSystemStart(context: Context) {
        try {
            marker(context).writeText(bootCount(context).toString())
        } catch (e: Exception) {
            Log.w(TAG, "could not record the system start", e)
        }
    }

    /** Whether the system started us this boot — and so whether a retry is ours to make. */
    fun wanted(context: Context): Boolean {
        val recorded =
            try {
                marker(context).takeIf { it.exists() }?.readText()?.trim()?.toIntOrNull()
            } catch (e: Exception) {
                Log.w(TAG, "could not read the system-start marker", e)
                null
            }
        val current = bootCount(context)
        if (recorded == null || current < 0 || recorded != current) {
            Log.i(
                TAG,
                "the system did not start us this boot (marker=$recorded, boot=$current); no retry",
            )
            return false
        }
        if (VpnService.prepare(context) != null) {
            Log.i(TAG, "no VPN consent; a background start cannot ask for it")
            return false
        }
        if (!File(context.applicationInfo.dataDir, AUTOSTART_FILENAME).exists()) {
            Log.i(TAG, "nothing has ever connected on this device; nothing to raise")
            return false
        }
        return true
    }

    fun schedule(context: Context) {
        val info =
            JobInfo.Builder(JOB_ID, ComponentName(context, BootRetryJobService::class.java))
                .setMinimumLatency(RETRY_LATENCY_MS)
                .setOverrideDeadline(RETRY_DEADLINE_MS)
                .build()
        val result = context.getSystemService(JobScheduler::class.java).schedule(info)
        if (result == JobScheduler.RESULT_SUCCESS) {
            Log.i(TAG, "retry scheduled in ${RETRY_LATENCY_MS / 1000} s")
        } else {
            Log.w(TAG, "could not schedule the retry: $result")
        }
    }

    /**
     * Start the service as the system would have — unless this process already knows better.
     *
     * A start the actor does not need is harmless (a tunnel that already satisfies the intent is
     * handed over, not rebuilt), but a needless start also brings the service foreground for
     * nothing, so it is skipped when the phase says anything is happening.
     */
    fun startIfIdle(context: Context, who: String) {
        val phase = VpnPhaseHolder.current()
        if (phase != VpnPhase.Off) {
            Log.i(TAG, "$who: the tunnel is $phase; nothing to do")
            return
        }
        Log.i(TAG, "$who: nothing is running; starting the service as the system would have")
        val intent =
            Intent(context, FloppaVpnService::class.java)
                .setAction(FloppaVpnService.ACTION_BOOT_RETRY)
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        } catch (e: Exception) {
            Log.e(TAG, "$who: could not start the service", e)
        }
    }
}

/**
 * `BOOT_COMPLETED`, in `:vpn`. Schedules [BootRetry] and gets out of the way.
 *
 * A background-restricted app (Settings → Battery → Restricted, or an OEM policy that sets the same
 * app-op — Onyx does, until "Stay Active in the Background" is ticked) never has its jobs run. The
 * receiver still fires for it, so that case is handled here directly: a start now, which helps
 * whenever the receiver runs after the launcher's cull and does no harm when it runs before. It
 * does *not* wait in the receiver for the cull to pass — `BOOT_COMPLETED` is delivered serially,
 * and a receiver that sleeps holds every app behind it.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        // Caught rather than thrown on, whatever it is: this receiver runs in the tunnel's
        // process, and the first build of it took that process down with a SecurityException
        // from a Settings read — on a boot where the tunnel would otherwise have survived.
        try {
            if (!BootRetry.wanted(context)) return
            BootRetry.schedule(context)
            val restricted =
                Build.VERSION.SDK_INT >= Build.VERSION_CODES.P &&
                    context.getSystemService(ActivityManager::class.java).isBackgroundRestricted
            if (restricted) {
                Log.w(TAG, "background-restricted: the job may never run, starting now instead")
                BootRetry.startIfIdle(context, "boot receiver")
            }
        } catch (e: Exception) {
            Log.e(TAG, "boot retry failed; leaving the process alone", e)
        }
    }

    private companion object {
        const val TAG = "FloppaVpnBoot"
    }
}

/** The delayed half of [BootRetry]. The work is one service start; there is nothing to stop. */
class BootRetryJobService : JobService() {
    override fun onStartJob(params: JobParameters): Boolean {
        BootRetry.startIfIdle(this, "boot retry job")
        return false
    }

    override fun onStopJob(params: JobParameters): Boolean = false
}
