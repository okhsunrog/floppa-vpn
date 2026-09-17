package dev.okhsunrog.floppavpn.vpn

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONArray
import org.json.JSONObject

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

/**
 * How long "connect with these rules" waits for a live tunnel to *start* changing before deciding
 * that it is not going to.
 *
 * The actor rebuilds a tunnel whose rules no longer match the intent and leaves one whose rules
 * already match exactly as it is — the second case is a success that never moves a phase, and
 * without this it would be reported as a fifteen-second timeout.
 */
private const val UNCHANGED_GRACE_MS = 2_000L

/** The phase as one lower-case word, which is what a script reads. */
private fun VpnPhase.wireName() =
    when (this) {
        VpnPhase.Off -> "off"
        VpnPhase.Busy -> "busy"
        VpnPhase.Connected -> "connected"
    }

/**
 * The split rules of the last successful connect, in one word: `all`, or `exclude:com.a,com.b`.
 *
 * Read from the autostart bundle rather than from the actor: it is written after every successful
 * connect and is what an autonomous start would rebuild from, so while a tunnel is up it describes
 * that tunnel, and while none is it describes the one that would come back. No JNI, no native
 * library, nothing to keep in step.
 *
 * One word rather than the broadcast's result extras, which would be the obvious home for it: `am
 * broadcast` prints a `Bundle` without unparcelling it (`Bundle[mParcelledData.dataSize=…]`), so
 * anything put there is invisible to the only caller this exists for.
 */
private fun recordedSplit(context: Context): String? =
    try {
        val file = File(context.applicationInfo.dataDir, AUTOSTART_FILENAME)
        if (!file.exists()) null
        else {
            val params = JSONObject(file.readText()).getJSONObject("params")
            val mode = params.getString("split_mode")
            val apps = params.optJSONArray("apps") ?: JSONArray()
            val list = List(apps.length()) { apps.getString(it) }
            if (list.isEmpty()) mode else "$mode:${list.joinToString(",")}"
        }
    } catch (e: Exception) {
        Log.w(TAG, "could not read the recorded split rules", e)
        null
    }

/**
 * Connect and disconnect the tunnel from a shell, for scripts and for CI.
 *
 * The service itself cannot be reached this way and should not be: it is `exported="false"` behind
 * `BIND_VPN_SERVICE`, so `am start-service` gets "Requires permission not exported". The Quick
 * Settings tile can be tapped remotely (`cmd statusbar click-tile`), but it is a *toggle* that has
 * to be added to Quick Settings first and whose refusals are "open the app", which is not something
 * a script can read. This is the deterministic version of the same requests:
 * ```
 * adb shell am broadcast -n dev.okhsunrog.floppa_vpn/dev.okhsunrog.floppavpn.vpn.AdbControlReceiver \
 *     -a dev.okhsunrog.floppavpn.adb.CONNECT --es split exclude --es apps com.foo,com.bar
 * ```
 *
 * The reply comes back as the ordered broadcast's result, which `am broadcast` prints: `Broadcast
 * completed: result=0, data="connected"`. `result=0` means the tunnel is in the state that was
 * asked for, `result=1` that it is not, and `data` is one word — the phase (`off`, `busy`,
 * `connected`), the rules `SPLIT` was asked for, or why the request could not be made
 * (`no-consent`, `nothing-to-raise`, `bad-split`, `bad-apps`, `start-refused`, `timeout`). See
 * `just vpn-connect`, `just vpn-disconnect`, `just vpn-status` and `just vpn-split`, which are this
 * with the quoting already done.
 *
 * **Split rules.** `CONNECT` takes an optional `--es split all|include|exclude`, with `--es apps
 * <comma-separated packages>` required by the latter two and refused by the first: an empty
 * `include` list is a tunnel nothing uses and an empty `exclude` list is `all` written the long
 * way, so both are far more likely to be a script's bug than a request. They are validated here,
 * before any service is started, and rejected by name. Given on a tunnel that is already up, they
 * are applied to it — the actor rebuilds a tunnel whose rules no longer match the intent — and a
 * request that matches what is already running is answered as soon as it is clear that nothing is
 * going to move. `SPLIT` reads them back — `all`, or `exclude:com.foo,com.bar` — from what the last
 * successful connect recorded.
 *
 * They are **not a setting**: the app's own rules live in the UI process's storage, which this
 * process cannot see. What a shell sets holds for the tunnel it starts and for the system starts
 * that rebuild it, and the next connect made from the app applies the app's rules again.
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

        /** Read-only: the split rules the last successful connect used. */
        const val ACTION_SPLIT = "dev.okhsunrog.floppavpn.adb.SPLIT"

        /** `all`, `include` or `exclude`. Optional; absent means "whatever was recorded". */
        const val EXTRA_SPLIT = "split"

        /**
         * Package names, comma-separated. Required by `include` and `exclude`, refused by `all`.
         */
        const val EXTRA_APPS = "apps"
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
                ACTION_SPLIT -> reply(RESULT_OK, recordedSplit(context) ?: "none")
                ACTION_CONNECT -> connect(context, intent)
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

    private fun connect(context: Context, request: Intent) {
        val params =
            when (val parsed = TunnelParamsArgs.parse(request)) {
                is TunnelParamsArgs.Rejected -> {
                    Log.w(TAG, "bad split rules: ${parsed.reason}")
                    reply(RESULT_NOT_DONE, parsed.reason)
                    return
                }
                is TunnelParamsArgs.Accepted -> parsed.json
            }
        val phase = VpnPhaseHolder.current()
        if (phase != VpnPhase.Off && params == null) {
            // Already up, or already on its way there, and nothing to change about it. Waiting is
            // still the useful thing to do: "connect" from a script means "have a tunnel by the
            // time this returns".
            Log.i(TAG, "already $phase")
            awaitConnect(moveRequired = false, graceMs = 0)
            return
        }
        if (phase == VpnPhase.Off) {
            val blocker = startBlocker(context)
            if (blocker != null) {
                Log.i(TAG, "cannot start: $blocker")
                reply(RESULT_NOT_DONE, blocker.toString())
                return
            }
        }
        // Armed before the start, so that a connect fast enough to finish inside it is not missed.
        // A live tunnel being given new rules has to be seen to *move* before its phase settling
        // at Connected means anything — and if it never moves, its rules already matched, which
        // the grace is there to say.
        val live = phase == VpnPhase.Connected
        awaitConnect(moveRequired = live, graceMs = if (live) UNCHANGED_GRACE_MS else 0)
        val start =
            Intent(context, FloppaVpnService::class.java)
                .setAction(FloppaVpnService.ACTION_ADB_START)
        if (params != null) start.putExtra(FloppaVpnService.EXTRA_TUNNEL_PARAMS, params)
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
     * the one we started from, and returning that would be answering before the question was asked.
     * [moveRequired] is what says whether "the phase we started from" was already `Connected`.
     */
    private fun awaitConnect(moveRequired: Boolean, graceMs: Long) {
        var moved = !moveRequired && VpnPhaseHolder.current() != VpnPhase.Off
        await(VpnPhase.Connected, graceMs) { phase ->
            if (phase == VpnPhase.Busy) moved = true
            moved && (phase == VpnPhase.Connected || phase == VpnPhase.Off)
        }
    }

    /**
     * Answer when [settled] says so, when [graceMs] passes with the phase still where it started,
     * or when the timeout runs out — and answer at once if the caller is not waiting for anything.
     */
    private fun await(target: VpnPhase, graceMs: Long = 0, settled: (VpnPhase) -> Boolean) {
        val phase = VpnPhaseHolder.current()
        if (!isOrderedBroadcast || settled(phase)) {
            reply(if (phase == target) RESULT_OK else RESULT_NOT_DONE, phase.wireName())
            return
        }
        wait = PendingWait(goAsync(), target, graceMs, settled).also { it.arm() }
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
     * the timeouts run on the main looper, and the request that armed the wait may fail on a third.
     * [finish] is what makes those safe together — the first to arrive answers, and the rest find
     * the wait already over.
     */
    private class PendingWait(
        private val pending: PendingResult,
        private val target: VpnPhase,
        private val graceMs: Long,
        private val settled: (VpnPhase) -> Boolean,
    ) {
        private val done = AtomicBoolean(false)
        private val handler = Handler(Looper.getMainLooper())
        private val start = VpnPhaseHolder.current()

        private val onPhaseChanged = Runnable {
            val phase = VpnPhaseHolder.current()
            if (settled(phase)) {
                finish(if (phase == target) RESULT_OK else RESULT_NOT_DONE, phase.wireName())
            }
        }

        /** Nothing moved, and nothing is going to: what is running is what was asked for. */
        private val onUnchanged = Runnable {
            val phase = VpnPhaseHolder.current()
            if (phase != start) return@Runnable
            Log.i(TAG, "nothing moved in ${graceMs / 1000} s; $phase is what was asked for")
            finish(if (phase == target) RESULT_OK else RESULT_NOT_DONE, phase.wireName())
        }

        private val onTimeout = Runnable {
            Log.w(TAG, "gave up waiting for $target after ${SETTLE_TIMEOUT_MS / 1000} s")
            finish(RESULT_NOT_DONE, "timeout")
        }

        fun arm() {
            VpnPhaseHolder.watch(onPhaseChanged)
            handler.postDelayed(onTimeout, SETTLE_TIMEOUT_MS)
            if (graceMs > 0) handler.postDelayed(onUnchanged, graceMs)
            // The phase may have settled between the caller's own check and the watch above.
            onPhaseChanged.run()
        }

        fun finish(code: Int, data: String) {
            if (!done.compareAndSet(false, true)) return
            VpnPhaseHolder.unwatch(onPhaseChanged)
            handler.removeCallbacks(onTimeout)
            handler.removeCallbacks(onUnchanged)
            Log.i(TAG, "reply: result=$code data=$data")
            pending.resultCode = code
            pending.resultData = data
            pending.finish()
        }
    }
}

/**
 * The `--es split` / `--es apps` extras, turned into the `TunnelParams` JSON the actor takes — or
 * into the one word that says what was wrong with them.
 *
 * Validated here, in the process that has neither started a service nor touched an intent yet, so a
 * script's typo costs a refusal rather than a tunnel built to rules nobody asked for.
 */
internal sealed interface TunnelParamsArgs {
    /** [json] is null when the caller named no rules at all, which is not an error. */
    data class Accepted(val json: String?) : TunnelParamsArgs

    data class Rejected(val reason: String) : TunnelParamsArgs

    companion object {
        private val MODES = setOf("all", "include", "exclude")

        fun parse(request: Intent): TunnelParamsArgs {
            val split = request.getStringExtra(AdbControlReceiver.EXTRA_SPLIT)?.trim()?.lowercase()
            val apps =
                request
                    .getStringExtra(AdbControlReceiver.EXTRA_APPS)
                    ?.split(',')
                    ?.map { it.trim() }
                    ?.filter { it.isNotEmpty() } ?: emptyList()
            if (split == null) {
                // Apps with no mode name nothing: "exclude these" and "only these" are opposite
                // tunnels and there is no defensible default between them.
                return if (apps.isEmpty()) Accepted(null) else Rejected("bad-split")
            }
            if (split !in MODES) return Rejected("bad-split")
            if (split == "all" && apps.isNotEmpty()) return Rejected("bad-apps")
            if (split != "all" && apps.isEmpty()) return Rejected("bad-apps")
            val json = JSONObject().put("split_mode", split).put("apps", JSONArray(apps)).toString()
            return Accepted(json)
        }
    }
}
