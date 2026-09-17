package dev.okhsunrog.floppavpn.vpn

import android.content.Context
import android.net.VpnService
import java.io.File

/**
 * The file `vpn/autostart.rs` writes after every successful connect and removes on a wipe. Its
 * existence is the cheapest possible answer to "has anything ever connected on this device", which
 * is all a start made with no UI needs from it.
 */
internal const val AUTOSTART_FILENAME = "autostart.json"

/**
 * What stands between a start made with no UI and a tunnel.
 *
 * Both have an answer that is "open the app" rather than "try and fail", which is why they are
 * checked before the service is started rather than left to the actor: a refusal that arrives after
 * the service is foreground has already raised a notification for nothing.
 */
internal enum class StartBlocker {
    /** Consent is missing, and only an activity can ask for it. */
    NoConsent,
    /** Nothing has ever connected here, so there is no intent to raise. */
    NothingToRaise;

    override fun toString() =
        when (this) {
            NoConsent -> "no-consent"
            NothingToRaise -> "nothing-to-raise"
        }
}

/**
 * Whether the tunnel can be started from somewhere with no activity — the tile, the boot retry, the
 * adb control surface — or `null` when nothing is in the way.
 *
 * [VpnService.prepare] is a question here and never a dialog: it can be *asked* from anywhere and
 * can only be *shown* from an activity, which none of these callers has.
 */
internal fun startBlocker(context: Context): StartBlocker? =
    when {
        VpnService.prepare(context) != null -> StartBlocker.NoConsent
        !File(context.applicationInfo.dataDir, AUTOSTART_FILENAME).exists() ->
            StartBlocker.NothingToRaise
        else -> null
    }
