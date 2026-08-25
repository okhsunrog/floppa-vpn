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
4. **A connected cycle reports what it stepped over.** The ladder can fail AmneziaWG's verification
   — its peer deleted server-side — and connect WireGuard a second later. The dead peer is now
   repaired quietly while the tunnel stays up.

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

### The device matrix 1b must pass

| step | expected |
| --- | --- |
| open the app | `:vpn` alive (bound), no notification |
| Connect | started + foreground, tunnel up |
| swipe-kill the UI | tunnel survives, reconnects on its own |
| tunnel dies in the background | detected and rebuilt with no UI process |
| Disconnect | foreground and started state dropped; process stays while the UI is bound |
| close the app after Disconnect | process dies |
| always-on start with no UI | intent raised to Up, tunnel built from the persisted intent |
| log out | configs and intent cleared, tunnel stopped, nothing comes back |
