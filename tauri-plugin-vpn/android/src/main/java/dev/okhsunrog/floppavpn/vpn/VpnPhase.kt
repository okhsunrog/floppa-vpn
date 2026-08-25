package dev.okhsunrog.floppavpn.vpn

import java.util.concurrent.CopyOnWriteArraySet

/** What the tunnel is doing, at the coarseness a Quick Settings tile can show. */
enum class VpnPhase {
    /** Nothing is running, and nothing is being started. */
    Off,
    /** Coming up, going down, or reconnecting — anything in motion. */
    Busy,
    /** A tunnel is up and carrying traffic. */
    Connected,
}

/**
 * The tunnel's phase, as this process last saw it.
 *
 * Deliberately *not* a member of [FloppaVpnService]'s companion: that companion's initialiser loads
 * the native library, and a Quick Settings panel opening is enough to create this process with no
 * service in it. Reading a companion property there would map the whole tunnel library to answer
 * "is anything running", every time the panel is pulled down.
 *
 * Written by the service — it is the only thing that knows — and read by [FloppaVpnTileService],
 * which lives in the same process. When the process does not exist, the tile reads the default,
 * [VpnPhase.Off], and that is the truth: no `:vpn` process, no tunnel.
 */
object VpnPhaseHolder {

    @Volatile private var phase: VpnPhase = VpnPhase.Off

    private val listeners = CopyOnWriteArraySet<Runnable>()

    fun current(): VpnPhase = phase

    fun watch(listener: Runnable) {
        listeners.add(listener)
    }

    fun unwatch(listener: Runnable) {
        listeners.remove(listener)
    }

    /**
     * Record a new phase and wake whoever is watching. Idempotent: the same phase notifies nobody.
     */
    fun publish(next: VpnPhase) {
        if (phase == next) return
        phase = next
        listeners.forEach { it.run() }
    }
}
