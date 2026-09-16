package dev.okhsunrog.floppavpn.vpn

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean

private const val TAG = "FloppaVpnAdb"

/** `result=` when the tunnel is in the state that was asked for. */
private const val RESULT_OK = 0

/** `result=` when it is not — refused, failed, or still moving when the wait ran out. */
private const val RESULT_NOT_DONE = 1

/**
 * How long a caller that waits may wait. Generous next to a connect (a second or two, or the
 * ladder's first protocol timing out), short next to a person's patience — and far inside the sixty
 * seconds the system allows a background broadcast, a deadline it enforces by killing the process.
 */
private const val SETTLE_TIMEOUT_MS = 15_000L

/** The phase as one lower-case word, which is what a script reads. */
private fun VpnPhase.wireName() =
    when (this) {
        VpnPhase.Off -> "off"
        VpnPhase.Busy -> "busy"
        VpnPhase.Connected -> "connected"
    }

/**
 * Connect and disconnect the tunnel from a shell, for scripts and for CI.
 *
 * The service itself cannot be reached this way and should not be: it is `exported="false"` behind
 * `BIND_VPN_SERVICE`, so `am start-service` gets "Requires permission not exported". The Quick
 * Settings tile can be tapped remotely (`cmd statusbar click-tile`), but it is a *toggle* that has
 * to be added to Quick Settings first and whose refusals are "open the app", which is not something
 * a script can read. This is the deterministic version of the same three requests:
 * ```
 * adb shell am broadcast -n dev.okhsunrog.floppa_vpn/dev.okhsunrog.floppavpn.vpn.AdbControlReceiver \
 *     -a dev.okhsunrog.floppavpn.adb.CONNECT
 * ```
 *
 * The reply comes back as the ordered broadcast's result, which `am broadcast` prints: `Broadcast
 * completed: result=0, data="connected"`. `result=0` means the tunnel is in the state that was
 * asked for, `result=1` that it is not, and `data` is one word — the phase (`off`, `busy`,
 * `connected`), or why the request could not be made (`no-consent`, `nothing-to-raise`,
 * `start-refused`, `timeout`). See `just vpn-connect`, `just vpn-disconnect` and `just vpn-status`,
 * which are this with the quoting already done.
 *
 * **Who may send this.** The receiver is exported — the sender is another app (`com.android.shell`)
 * — and guarded by `android.permission.DUMP`, which the shell holds and an ordinary app cannot
 * obtain: it is `signature|privileged|development`, so granting it to a third-party app itself
 * takes adb. Anyone who can reach this can already tap the tile with `input`, so the guard is about
 * keeping *other apps* out, not about keeping a person with a cable out.
 *
 * **Why it lives in `:vpn`.** For the reason the tile does: the phase it reports is a process-local
 * read, and no `:vpn` process means no tunnel, which is exactly the answer `STATUS` should give.
 * Asking for the status when nothing is running does create the process — briefly, with no service
 * in it and no native library loaded — and it goes away again on its own.
 *
 * **Waiting.** An ordered broadcast is answered once the request has settled or fifteen seconds
 * have passed, whichever comes first, so a script can connect and then use the tunnel on the next
 * line. `am broadcast --async` sends it unordered instead, and then nothing here waits for
 * anything: the request is made and the receiver returns.
 */
class AdbControlReceiver : BroadcastReceiver() {

    companion object {
        const val ACTION_CONNECT = "dev.okhsunrog.floppavpn.adb.CONNECT"
        const val ACTION_DISCONNECT = "dev.okhsunrog.floppavpn.adb.DISCONNECT"
        const val ACTION_STATUS = "dev.okhsunrog.floppavpn.adb.STATUS"
    }

    /** The wait this request armed, if it armed one. It owns the reply once it exists. */
    private var wait: PendingWait? = null

    override fun onReceive(context: Context, intent: Intent) {
        Log.i(TAG, "request: ${intent.action} (ordered=$isOrderedBroadcast)")
        // Nothing here may throw: this receiver runs in the tunnel's process, and an exception out
        // of a receiver takes its process — and the tunnel it is carrying — with it.
        try {
            when (intent.action) {
                ACTION_STATUS -> reply(RESULT_OK, VpnPhaseHolder.current().wireName())
                ACTION_CONNECT -> connect(context)
                ACTION_DISCONNECT -> disconnect(context)
                else -> {
                    Log.w(TAG, "unknown action: ${intent.action}")
                    reply(RESULT_NOT_DONE, "unknown-action")
                }
            }
        } catch (e: Exception) {
            Log.e(TAG, "the request failed", e)
            reply(RESULT_NOT_DONE, "error")
        }
    }

    private fun connect(context: Context) {
        val phase = VpnPhaseHolder.current()
        if (phase != VpnPhase.Off) {
            // Already up, or already on its way there. Waiting is still the useful thing to do:
            // "connect" from a script means "have a tunnel by the time this returns".
            Log.i(TAG, "already $phase")
            awaitConnect()
            return
        }
        val blocker = startBlocker(context)
        if (blocker != null) {
            Log.i(TAG, "cannot start: $blocker")
            reply(RESULT_NOT_DONE, blocker.toString())
            return
        }
        // Armed before the start, so that a connect fast enough to finish inside it is not missed.
        // The actor raises the intent from what the last successful connect recorded, exactly as
        // it does for the tile and for always-on.
        awaitConnect()
        val start =
            Intent(context, FloppaVpnService::class.java)
                .setAction(FloppaVpnService.ACTION_ADB_START)
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(start)
            } else {
                context.startService(start)
            }
        } catch (e: Exception) {
            // The one that is expected here: from Android 12 a background start of a foreground
            // service is refused unless the app is exempt — being off the battery optimisations is
            // one way, and the app asks for that on its first run. Said plainly, because the fix
            // is on the device and not in the script.
            Log.e(TAG, "could not start the service", e)
            reply(RESULT_NOT_DONE, "start-refused")
        }
    }

    private fun disconnect(context: Context) {
        if (VpnPhaseHolder.current() == VpnPhase.Off) {
            Log.i(TAG, "nothing is running")
            reply(RESULT_OK, VpnPhase.Off.wireName())
            return
        }
        await(VpnPhase.Off) { it == VpnPhase.Off }
        val stop =
            Intent(context, FloppaVpnService::class.java).setAction(FloppaVpnService.ACTION_STOP)
        try {
            // `startService`, not `startForegroundService`: the service is already foreground —
            // nothing running is the branch above — and it may stop within seconds of this, which
            // is precisely the shape the foreground-start deadline punishes.
            context.startService(stop)
        } catch (e: Exception) {
            Log.e(TAG, "could not ask the tunnel to stop", e)
            reply(RESULT_NOT_DONE, "stop-refused")
        }
    }

    /**
     * Wait for a connect to resolve one way or the other.
     *
     * [VpnPhase.Connected] is the answer we are after; [VpnPhase.Off] is one too, but only once
     * something has visibly happened — until the actor publishes [VpnPhase.Busy] the phase is still
     * the `Off` we started from, and returning that would be answering before the question was
     * asked.
     */
    private fun awaitConnect() {
        var moved = VpnPhaseHolder.current() != VpnPhase.Off
        await(VpnPhase.Connected) { phase ->
            if (phase == VpnPhase.Busy) moved = true
            phase == VpnPhase.Connected || (moved && phase == VpnPhase.Off)
        }
    }

    /**
     * Answer when [settled] says so, or when the timeout runs out, whichever happens first — and
     * answer at once if the caller is not waiting for anything.
     */
    private fun await(target: VpnPhase, settled: (VpnPhase) -> Boolean) {
        val phase = VpnPhaseHolder.current()
        if (!isOrderedBroadcast || settled(phase)) {
            reply(if (phase == target) RESULT_OK else RESULT_NOT_DONE, phase.wireName())
            return
        }
        wait = PendingWait(goAsync(), target, settled).also { it.arm() }
    }

    /**
     * Answer the caller. Once a wait exists it owns the reply — `goAsync()` has taken the result
     * away from this receiver, and setting one here would throw — so every path goes through this.
     */
    private fun reply(code: Int, data: String) {
        val pending = wait
        if (pending != null) {
            pending.finish(code, data)
            return
        }
        Log.i(TAG, "reply: result=$code data=$data")
        if (isOrderedBroadcast) {
            resultCode = code
            resultData = data
        }
    }

    /**
     * One caller waiting for the phase to settle.
     *
     * The phase is published from whatever thread the actor is on, so the listener may run there;
     * the timeout runs on the main looper, and the request that armed the wait may fail on a third.
     * [finish] is what makes those safe together — the first to arrive answers, and the rest find
     * the wait already over.
     */
    private class PendingWait(
        private val pending: PendingResult,
        private val target: VpnPhase,
        private val settled: (VpnPhase) -> Boolean,
    ) {
        private val done = AtomicBoolean(false)
        private val handler = Handler(Looper.getMainLooper())

        private val onPhaseChanged = Runnable {
            val phase = VpnPhaseHolder.current()
            if (settled(phase)) {
                finish(if (phase == target) RESULT_OK else RESULT_NOT_DONE, phase.wireName())
            }
        }

        private val onTimeout = Runnable {
            Log.w(TAG, "gave up waiting for $target after ${SETTLE_TIMEOUT_MS / 1000} s")
            finish(RESULT_NOT_DONE, "timeout")
        }

        fun arm() {
            VpnPhaseHolder.watch(onPhaseChanged)
            handler.postDelayed(onTimeout, SETTLE_TIMEOUT_MS)
            // The phase may have settled between the caller's own check and the watch above.
            onPhaseChanged.run()
        }

        fun finish(code: Int, data: String) {
            if (!done.compareAndSet(false, true)) return
            VpnPhaseHolder.unwatch(onPhaseChanged)
            handler.removeCallbacks(onTimeout)
            Log.i(TAG, "reply: result=$code data=$data")
            pending.resultCode = code
            pending.resultData = data
            pending.finish()
        }
    }
}
