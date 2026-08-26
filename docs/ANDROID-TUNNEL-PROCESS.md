# Moving the tunnel actor into the `:vpn` process

Status: **stage 0 and stages 1a–1b landed; not yet device-tested** (branch
`feat/vpn-process-actor`). Stage 1c — retiring what the move made dead — is still open.

## Why

On Android the app is two processes: the Tauri UI, and `:vpn`, which owns the tunnel. Today the
*decisions* — the intent, the status, the reconnect ladder, the whole decision table — live in the
UI process, and the tunnel lives in the other one. That split is the reason reconnection is not
reliable:

- **The UI process is frozen in the background.** Android's cached-app freezer stops it entirely:
  no timers, no observation polling, no reconnect. A tunnel that dies while the phone is in a
  pocket stays dead until the user opens the app. This is a platform behaviour, not a bug in the
  actor — on unfreeze every timer fires correctly.
- **The UI process is killed freely.** `:vpn` outlives it by design, so after a swipe-close the
  tunnel runs with nobody watching it at all.
- **Two authorities.** `:vpn` can start a tunnel by itself (always-on, boot, lockdown) while the
  UI's intent says Down. That is modelled today by *adoption* — a decision table row that promotes
  the UI's intent to match what the system did. It works, but it exists only because the intent is
  in the wrong process.

The fix is to put the actor where the tunnel is. `:vpn` is a foreground service while a tunnel is
up: it is never frozen, and it is the thing Android restarts.

## Stage 0 — liveness and reflexes (landed)

Independent of where the actor lives, and useful on desktop too.

1. **The owner reports how long the far side has been silent.** For the WireGuard family that is
   the *handshake* age, not the last inbound packet: an idle-but-healthy tunnel receives nothing
   for minutes, while `PersistentKeepalive` keeps it sending and a send past `REKEY_AFTER_TIME`
   starts a new handshake — so a healthy peer rekeys about every two minutes whatever the user is
   doing. A peer whose session has expired reports no handshake at all, which is what a peer
   deleted on the server looks like once its last session ages out. VLESS has no handshake and
   uses inbound traffic, refreshed by its ping.
2. **Silence buys a probe, never a verdict** (table rows 17a–17c). Past the bound the far side is
   asked to prove it is there — a forced rehandshake through gotatun's `suspend`/`resume`, a ping
   for VLESS — and only silence that survives the probe's grace ends the tunnel. A phone that spent
   an hour asleep and a config with no keepalive are both silent with nothing wrong; tearing those
   down on age alone would be a new bug rather than a fix.
3. **The network-change reflex.** `:vpn` follows the default network and rebinds the tunnel's
   socket in place when it changes. Nothing watched this before, and a Wi-Fi ⇄ mobile roam leaves
   the socket bound to a network that no longer exists — the tunnel looks up and carries nothing.
   The reflex never changes *what* is running, so it cannot fight the actor's recovery, which
   starts a whole cycle and is minutes away. `setUnderlyingNetworks` is finally set too, which is
   what makes traffic accounting and connectivity checks correct for the apps inside the tunnel.
4. **An outage is waited out, not spent.** The reflex above handles a *roam* — there is another
   network to move onto. An **outage** leaves none, and it used to cost the tunnel permanently: the
   peer went quiet, row 17c called it dead, and the reconnect cycle spent its whole budget on
   attempts that could not have worked before demoting the intent. When the signal came back
   nothing was left to notice. `Link::{Online, Offline, Unknown}` closes it with two gates and one
   asymmetry:

   - `connecting()` — the single choke point every attempt passes through — **parks** the cycle
     while the link is `Offline`. No pass burnt, no protocol stepped over, no effect fired. It
     waits in `Retrying` with `resume_at` already past, so the link report itself is what resumes
     it: the report arrives as a command on the actor's one channel, and delivering a command *is*
     a table pass. No timer, no deadline to guess.
   - Row 17 does not judge silence at all while `Offline`, and treats it as *answering* rather than
     merely skipping — which clears `probing_since`. Pausing the clock instead would leave it
     long-expired when the network returned, and 17c would fire one second before the rebind
     reflex fixed the socket.
   - `Unknown` **gates nothing.** It is the state every platform without a watcher stays in
     forever, so the desktop and the CLI behave exactly as they did; the whole existing reconcile
     suite runs at `Unknown` and is the proof. `Offline` is only ever a positive, live report —
     never inferred from a failure, a timeout, or a missing API.

   A cold connect is parked on the same terms as a reconnect. Failing fast would be defensible for
   a person pressing Connect in airplane mode, but the same path serves the system start at boot,
   where the service routinely runs before Wi-Fi has associated — and failing fast there demotes
   the intent under an always-on lockdown.

   The watch had to move with it: Kotlin registers the callback in `onCreate`, for the life of the
   service instance, not per generation. A watch that lived and died with the tunnel could never
   report the one thing that matters here — that the network is back when no tunnel exists.
   `setUnderlyingNetworks` and the rebind keep their generation guard; only the link report is
   unconditional. Stopping the watch reports `Unknown`, because `Offline` is a live report and one
   with nobody left to update it would park the next connect for ever.

   **`registerBestMatchingNetworkCallback` does not tell you what it stopped tracking.** It reports
   one network — the best — and sends `onLost` only for a network that was the best *and went
   away*, never for one that merely lost to a better one. So its `onAvailable` has to be read as a
   *replacement*, not an addition. Below API 31 the plain registration is the opposite: every match
   is reported and every loss is too, so there a set is the right shape. Treating the two alike is
   not a tidy simplification, it is a silent failure — Wi-Fi returning alongside mobile left the
   set holding both, and when every network then went away the set still held the mobile one it had
   never been told about. Nothing reported the outage and the gate above was dead on the device
   while every host test passed.

   The tell, on a device, was an *absent* log line: no `link=Online` where one was due, which is
   what says `wasEmpty` was false when it should have been true.

   **Not covered:** desktop and the CLI. They stay at `Unknown`, so the budget burn on a
   disconnected machine remains until something watches netlink or the routing table there.

5. **A connected cycle reports what it stepped over.** The ladder can fail AmneziaWG's verification
   — its peer deleted server-side — and connect WireGuard a second later. The dead peer is now
   repaired quietly while the tunnel stays up, by `provision/watcher.rs` in the process that holds
   the actor.

Stage 0 leaves one hole, and it is the reason for stage 1: *detection* runs in the actor, and the
actor is still in the frozen process.

## Stage 1 — the move

### Where things end up

```text
UI process                                  :vpn process (foreground while a tunnel is up)
┌────────────────────────────┐             ┌──────────────────────────────────────────────┐
│ Tauri commands             │             │ TunnelActor  (intent, status, ladder, table) │
│   └─ remote TunnelHandle ──┼── RPC ────► │ ConfigStore  (the one writer)                │
│ consent dialog (prepare)   │             │ InProcessBackend → TunnelManager → gotatun   │
│ starts the service         │             │ ServiceHost  → JNI → FloppaVpnService        │
│ renders TunnelState        │◄── state ───┤ notification text = the actor's own phase    │
└────────────────────────────┘             └──────────────────────────────────────────────┘
```

Desktop is unchanged: the same actor, hosted in-process, with a local handle.

### The three decisions this rests on

**1. The wire becomes JSON.** `TunnelState` transitively contains `CycleOutcome`, `AttemptError`,
`BackendError` and `IntentError`, every one of them an internally tagged enum — precisely the shape
bincode cannot decode, which is the bug that shipped once already (the AmneziaWG I-slots). Mirroring
all of that into bincode-safe wire types would be a permanent tax on every future field. The
transport moves to `tokio_serde::formats::Json`, which is self-describing, and the rule the owner
set keeps its force in a generalised form: **every type on the wire round-trips through the wire's
own codec, in every variant.** The one JSON-specific hazard is non-finite floats — `serde_json`
writes them as `null` and then fails to read them back. The only floats on the wire are the
per-second rates, and `SpeedTracker` never divides by an interval below 100 ms, so they are always
finite; the wire tests pin it.

**2. `:vpn` owns the config store, and the UI binds the process to read it.** The actor and the
store cannot be in different processes — the store is what resolves a probe order, names the
preferred protocol and is published inside every `TunnelState`. So the store moves, and the UI
reaches it over the same RPC. To read configs without a tunnel running, the UI **binds** the
service (`BIND_AUTO_CREATE`, no Binder needed — the socket does the talking): binding starts the
process without any notification, and the service goes foreground only when a tunnel actually
comes up.

**3. The system is a second principal, and it wins by restarting.** Adoption exists today because
the OS can start a tunnel the UI's intent does not cover. With one authority the axis has to be
modelled directly:

| who | how it arrives | effect on the intent |
| --- | --- | --- |
| the user | RPC `set_intent` | exactly what was asked |
| the system | `onStartCommand` with the VPN action, boot, or lockdown | raised to Up from the last-good order and rules |
| a wipe | RPC `Forget` | Down, configs cleared — nothing can come back |

An ordinary Disconnect still stops the tunnel, and if always-on is on Android will start the
service again and the tunnel with it. That is the current behaviour and it is the right one: whether
an always-on tunnel comes back is the system toggle's decision, not the app's, and the app fighting
it is what produced a restart loop once before. The persisted intent replaces `autostart.json`;
`Forget` clearing it is what keeps a logged-out account's tunnel from coming back.

### Consent

`VpnService.prepare(context)` can be *checked* from anywhere and can only be *shown* from an
activity. So the UI asks — before it requests a tunnel, where it has an activity and a person
looking at it — and the actor only ever *checks*. Consent that is missing is a refusal, which the
ladder already treats as fatal for the whole cycle.

That is deliberately not a "waiting for consent" state. A reconnect at three in the morning cannot
show a dialog whatever it does, so parking would mean holding a tunnel-shaped promise nobody can
fulfil until the user next opens the app — and the app opening is exactly when the UI asks anyway.
Revocation is the same story from the other side: the user has said no, and an app that keeps a
pending request alive across it is arguing.

### Always-on, and what it does not mean

Always-on VPN does not mean "the system re-establishes the tunnel whenever it drops". `isAlwaysOn`
says what it does mean: *"the system ensures that the service is always running by restarting it
when necessary, e.g. after reboot"* — boot, an app update, a process that died. A deliberate stop
is respected, and Android's own guide says so: *"A person using the device can stop your service by
using your app's UI. Stop the service instead of just closing the connection."* Which is why
`shutdownService()` calls `stopSelf` rather than only closing the descriptor.

Verified on a device with always-on enabled: disconnect from the tile, and two minutes later the
tunnel is still down and the service still gone.

So the pieces line up like this. Every start says who asked — `ACTION_KEEP_ALIVE` from the UI,
`ACTION_TILE_START` from the tile, and an unflagged `android.net.VpnService` from the system. That
flagging is the trick the VPN guide recommends for telling them apart, and it stays: it is the only
thing that answers *who issued this start*. `autostart.json` is written on every successful connect
and removed only by a wipe, so the one non-obvious consequence is that **a manual disconnect does
not survive a reboot** while always-on is on — which is precisely what the user asked Android for.

**`isAlwaysOn()` does not answer that question, and an earlier version of this document said it
did.** Its implementation is `isCallerCurrentAlwaysOnVpnApp()` — the system is asked whether *this
app is configured* as the always-on VPN, which is equally true when a person presses Connect in the
app. The two facts coexist: the intent flag says who asked for a start, `isAlwaysOn` says how the
system is configured. Nothing gates the unflagged-start raise on it, deliberately — a `START_STICKY`
restart after the process died also arrives unflagged, with always-on switched off, and raising the
intent there is crash recovery.

Both queries are now read, together, and published as `SystemVpnMode`:

| | what it means |
| --- | --- |
| `Unknown` | nobody could say. Both queries are API 29 and `minSdk` is 24, so an older device can *be* the always-on VPN and be unable to answer; a failed call lands here too. Rendered as silence, never as "no". |
| `Off` | the system does not run this app as its always-on VPN. |
| `AlwaysOn` | the system restarts the *service* when it needs to. Not "re-establishes a tunnel that drops". |
| `Lockdown` | as above, and apps may not bypass the VPN. |

Nested rather than two booleans because AOSP nests them — `isLockdownEnabled` is documented as
*"running in always-on VPN **lockdown** mode"* — so "lockdown without always-on" is unrepresentable,
as it should be. Kotlin pushes the whole answer in one call (`nativeVpnModeChanged`); separate
pushes could tear it.

**The queries only answer for the VPN's owner, and that is not obvious from their names.**
`isCallerCurrentAlwaysOnVpnApp()` is `getVpnIfOwner() != null && vpn.getAlwaysOn()`, and there is no
owner until `establish()` has run. Asked any earlier, both return `false` — meaning *"you are not
the owner"*, not *"always-on is off"*. The first build of this asked from `onCreate` and published
`Off` from a service the system had itself started for always-on. So the read happens **after
`establish()`**, is retracted to `Unknown` when the tunnel goes, and Kotlin sends a `known` flag so
"could not ask" can never arrive as a definite no — the same discipline as `Link::Unknown`, learned
the same way. It costs nothing: the two things the mode is shown for are both only visible while a
tunnel is up.

**Lockdown is no longer invisible.** With "Block connections without VPN" on, a manual disconnect
leaves the device with no network at all, and the card now says so above the disconnect button
before it is pressed. Nothing in the decision table reads the mode: what the system does about a
tunnel that stops is the system's business, and the change here is that a person is told what
stopping will cost.

**Lockdown cancels split tunneling outright, and the system does not mention it.**
`Vpn.setVpnForcedLocked` builds the blocked UID ranges from `mLockdownAllowlist + mPackage` and
passes `allowedApplications: null` — the VPN's own app lists are not consulted at all. So an app
excluded from the tunnel does not fall back to the plain network the way this app's split-tunnelling
card promises; it gets nothing. Confirmed on the device, with 46 apps excluded and lockdown on:

```
Lockdown filtering rules:
    UIDs: 1-10320        our app is 10321, and 10321 is the only gap
    UIDs: 10322-20320    (20321 is the same app in the second profile)
    UIDs: 20322-99999
```

An excluded app — Ozon, uid 10338 — sits inside the second range. The only way out is Android's own
`always_on_vpn_lockdown_whitelist`, which is set in Settings and not by us. The split-tunnelling card
says so when the mode is `Lockdown`: it is not a caveat about those settings, it is the reason they
are currently doing nothing.

The residual window is a Settings toggle that somehow does not reconfigure the VPN — changing
always-on or lockdown normally restarts the service, which is a push. A stale value costs a wrong
caption, never a wrong decision. A read taken inside the disconnect flow itself is where a
genuinely fresh check would go, if evidence ever shows the window matters.

### Staging

- **1a — make the actor host-agnostic.** No `tauri::AppHandle` in the actor core: spawning is
  injected, and everything the Android ladder does through the plugin becomes a `ServiceHost`
  trait (check consent, establish a TUN, stop, set the notification). `TunnelHandle` becomes an
  interface with a local implementation. Zero behaviour change, fully host-testable.
- **1b — run it in `:vpn`.** The RPC grows the actor's command surface plus a long-polled
  `state_since(seq)`; the UI gets the remote handle. Kotlin gains bind-for-lifetime and
  establish-on-demand: the start intent no longer carries a TUN spec, because the actor derives it.
- **1c — retire what the move makes dead.** With one actor in the tunnel's own process, several
  things exist only to describe a world that no longer occurs:
  - `Started.autonomous` and `RunningTunnel.autonomous` are always false — every tunnel is started
    by the actor, and a start the system issues reaches it as an intent like any other.
  - Table row **2a** (adopt a tunnel the system started) is therefore unreachable, and so is the
    `AdoptAutonomous` effect it emits. Row **2b** — a tunnel with no intent — can now only be
    reached by a bug: the tunnel dies with the process that decides about it.
  - Service *generations* still earn their keep (a descriptor arrives asynchronously and must name
    what it answers), but they no longer need a reserved range or a persisted counter.
  - `TunnelInfo`, `RunningInfo` and the `starting`/`tun_ready` fields the old RPC carried are gone
    already; what remains is the `ServiceRegistry` that replaced them.

### The device matrix

Run on a Pixel 8 Pro, release build, 25 August 2026.

| step | result |
| --- | --- |
| open the app | `:vpn` alive (bound), no notification — **pass** |
| Connect | started + foreground, connected in 0.8 s — **pass** |
| kill the UI process | tunnel keeps carrying traffic — **pass** |
| Wi-Fi ⇄ mobile roam | one rebind each way, no break (16 ms → 165 ms → 16 ms) — **pass** |
| network cut for 3 min, no UI | silence at 180 s → probe → lost at +20 s → ladder retried → back 8 s after the network returned — **pass** |
| Disconnect | notification gone, process stays while bound — **pass** |
| close the app after Disconnect | both processes die — **pass** |
| always-on start with no UI | intent raised from the persisted one, 0.8 s; a UI opened later shows the truth with no adoption — **pass** |
| `:vpn` restarts under a live UI | *not runnable* — `am kill`/`am crash` are refused for a release build; covered by the host test in `rpc_server.rs` |
| swipe-kill after a *fallback* connect | *not run* — forcing a mid-ladder failure needs a server-side change |
| peer deleted on the server, tunnel running | silence at 180 s → probe → `PeerSilent` → AmneziaWG failed verification → WireGuard carried it → the dead peer was recreated in the background, tunnel untouched, and connects again — **pass** (three runs; the first two found the two frontend bugs below) |
| log out | *not run* — it signs the owner out |

Two defects the device found, both fixed: the network callback watched the *default* network, which
after the tunnel comes up is our own VPN — so a real Wi-Fi to mobile switch produced no callback at
all; and a cold start by the system stood the service down mid-start, because a freshly booted actor
publishes Disconnected before anything has asked it for a tunnel.

Replaying the incident that started all this — a peer deleted on the server while the tunnel was
up — took three runs, because the actor half worked immediately and the frontend half was broken
in two separate ways. Both were invisible until a *background reconnect that succeeds* became a
thing that happens, which is exactly what this branch introduced.

(The frontend half is gone now: the repair lives in `provision/watcher.rs`, in the process that
holds the actor, and the frontend neither repairs nor knows about repairs. The two bugs below are
kept because the second one — outcome identity — is a property of the actor and still load-bearing
for whoever reads outcomes.)

### The replay, with nobody looking

Run on 2026-08-26 with the app closed for the whole of it — only `:vpn` in the process list:

| | |
|---|---|
| 14:39:53 | the WireGuard peer deleted on the server, the app closed |
| 14:41:32 | silence noticed, probing |
| 14:41:52 | ruled `PeerSilent`, unwinding |
| 14:42:10 | WireGuard: `no handshake / no connectivity through the tunnel` |
| 14:42:23 | `Connected { AmneziaWg, failures: [WireGuard VerifyFailed] }` |
| 14:42:23 | `a peer the ladder stepped over was replaced protocol=wireguard` |

Two and a half minutes from deletion to a working tunnel and a replaced peer, with the phone in a
pocket. The first attempt at this run died instead, and is why `floppa-api-client` configures its
own TLS roots: `reqwest` on rustls defaults to the *platform* verifier, which on Android needs a
JNI handshake with the system trust store that nothing had performed — so the repair panicked with
"Expect rustls-platform-verifier to be initialized" the moment it reached for the server.

First: `VpnCard`'s outcome watcher returned early unless the outcome "needed attention", and a
cycle that connected does not — so the repair could never run for the reconnects it exists for.

Second, and deeper: the sticky outcome was deduplicated by `{ epoch, outcome }`, and a reconnect
runs under the *same intent*. A tunnel that dropped and came back therefore reports `connected`
twice under one epoch with the same tag, and the second — the one carrying "a protocol was stepped
over" — was swallowed as a duplicate of the connect the user had pressed minutes earlier. The
actor now stamps every outcome with a serial, and that is the whole key.

One gap it also found: an outage longer than the reconnect budget ends in `LostGaveUp` and the
intent is demoted, so only always-on brings the tunnel back afterwards. "There is no network at
all" is arguably not a failure worth spending a pass on.
