# Driving Floppa VPN from adb

A self-contained reference for scripts, CI and agents working in *other* projects: how to turn this
VPN on and off, and how to set its split-tunnelling rules, on an Android device over adb. Nothing
here needs this repository checked out — the `just` recipes are conveniences, and every one of them
is a single `am broadcast` written out below.

For why it is built this way, see `ANDROID-TUNNEL-PROCESS.md`, "Driving it from a shell".

## Requirements

- A device connected over adb (`adb devices` shows it as `device`).
- Floppa VPN installed, and **connected at least once from the app** — a shell can only ask for what
  the last successful connect recorded. Otherwise every connect answers `nothing-to-raise`.
- VPN consent granted (the system dialog, shown once by the app). Otherwise: `no-consent`.
- Battery optimisation off for the app (the app asks for this on first run). Without the exemption
  Android refuses a background start of a foreground service and the answer is `start-refused`.

No root. Nothing else to install.

## The four commands

```bash
RCV=dev.okhsunrog.floppa_vpn/dev.okhsunrog.floppavpn.vpn.AdbControlReceiver
BC="adb shell am broadcast -f 0x01000000 -n $RCV -a dev.okhsunrog.floppavpn.adb"

$BC.STATUS        # off | busy | connected
$BC.CONNECT       # connect and wait for it to settle
$BC.DISCONNECT    # disconnect and wait for it to settle
$BC.SPLIT         # the split rules the last successful connect used
```

`-f 0x01000000` is `FLAG_INCLUDE_STOPPED_PACKAGES`: a freshly installed or force-stopped app
receives nothing without it, which is exactly the state a CI run starts from.

The reply is the last line `am` prints:

```
Broadcast completed: result=0, data="connected"
```

- **`result`** — `0` when the tunnel is in the state that was asked for, `1` when it is not.
- **`data`** — one word:
  - a phase: `off`, `busy`, `connected`;
  - the rules, for `SPLIT`: `all`, `exclude:com.foo,com.bar`, `include:com.foo`, or `none` when
    nothing has ever connected;
  - or why the request could not be made: `no-consent`, `nothing-to-raise`, `bad-split`, `bad-apps`,
    `start-refused`, `timeout`, `unknown-action`, `error`.

`CONNECT` and `DISCONNECT` **block** until the tunnel settles (or 15 s pass), so the next line of a
script can use the tunnel. Both are idempotent: connecting a connected tunnel answers `connected`
immediately. Adding `--async` to `am broadcast` sends it unordered — the request is made, nothing
waits, and there is no reply to read.

## Split tunnelling

`CONNECT` optionally carries the rules for the tunnel it starts:

```bash
# only these apps go through the VPN
$BC.CONNECT --es split include --es apps com.example.myapp

# everything except these
$BC.CONNECT --es split exclude --es apps com.example.myapp,com.android.chrome

# everything
$BC.CONNECT --es split all
```

- `--es apps` is a comma-separated list of package names. It is **required** by `include` and
  `exclude`, and **refused** with `all`: an empty `include` list is a tunnel nothing uses and an
  empty `exclude` list is `all` written the long way, so both are rejected (`bad-apps`) rather than
  guessed at. An unknown mode is `bad-split`. Neither ever starts a tunnel.
- Given for a tunnel **that is already up**, the rules are applied to it — it is rebuilt with them,
  and the command returns when it is back up. Rules that already match return as soon as it is clear
  nothing is going to move (about two seconds).
- Everything else about the connect — which protocol, which server — is what the last successful
  connect used. A shell cannot choose those.
- Read them back with `$BC.SPLIT`.

**These are not the app's setting.** The app keeps its own split rules in its UI process; this
process cannot see them. What a shell sets holds for the tunnel it starts and for the tunnels
Android rebuilds on its own afterwards (always-on, boot) — until someone presses Connect in the app,
which applies the app's own rules again. Fine for automated testing, not a way to configure the app.

## Ready-made bash wrapper

```bash
RCV=dev.okhsunrog.floppa_vpn/dev.okhsunrog.floppavpn.vpn.AdbControlReceiver

# vpn <ACTION> [extra am args...]; prints the answer, exits 0 only if the request was carried out
vpn() {
  local action=$1; shift
  local out
  out=$(adb shell am broadcast -f 0x01000000 -n "$RCV" \
        -a "dev.okhsunrog.floppavpn.adb.$action" "$@" 2>&1) || return 1
  local data=${out##*data=\"}; data=${data%%\"*}
  printf '%s\n' "$data"
  [[ "$out" == *"result=0"* ]]
}

vpn STATUS
vpn CONNECT --es split exclude --es apps com.example.myapp || echo "VPN did not come up"
vpn SPLIT
vpn DISCONNECT
```

A typical test run:

```bash
vpn CONNECT --es split include --es apps com.example.myapp || exit 1
./run-my-tests.sh          # the app's traffic is inside the tunnel, everything else is not
vpn DISCONNECT
```

## Verifying and debugging independently

```bash
# is the tunnel really up, according to the system?
adb shell dumpsys connectivity | grep -q "VPN CONNECTED extra: VPN:dev.okhsunrog.floppa_vpn" \
  && echo up || echo down

# which apps the running tunnel actually covers — the proof that split rules took effect
adb shell dumpsys connectivity | grep "VPN:dev.okhsunrog.floppa_vpn" | grep -o "Uids: <{[^}]*}>"
# `include com.android.chrome` shows one uid (plus its work-profile twin); `all` shows {0-99999}

# what the app itself logged about the request
adb logcat -d -s FloppaVpnAdb FloppaVpnService | tail -30
```

`FloppaVpnAdb` logs every request and every reply, so a command that answers something unexpected
can be traced without guessing.

## If you own the repository checkout

```bash
just vpn-status
just vpn-connect                            # optionally: just vpn-connect exclude com.foo,com.bar
just vpn-split
just vpn-disconnect
```

Each prints the answer and exits with the same verdict as `result`. `ADB_DEVICE=<serial>` or a
trailing `device=<serial>` argument targets one of several attached devices.

## What does not work, and why

- **`am start-service` / `am startservice` on the VPN service.** It is `exported="false"` behind
  `BIND_VPN_SERVICE`, so a shell gets `Error: Requires permission not exported from uid …`. Only uid
  0 passes that check, so it works under `su` on a rooted device and nowhere else.
- **Writing `Settings.Secure.always_on_vpn_app`.** The shell holds `WRITE_SECURE_SETTINGS` and the
  write succeeds, but nothing re-reads that key when it changes; all it achieves is a disagreement
  between the setting and the running system.
- **Tapping the Quick Settings tile** (`adb shell cmd statusbar click-tile
  dev.okhsunrog.floppa_vpn/dev.okhsunrog.floppavpn.vpn.FloppaVpnTileService`) does work without
  root, and is a genuine end-to-end path. It is a *toggle*, though: it needs the tile to be in Quick
  Settings already, it says nothing back, and its refusals open the app rather than report anything.
  Use it to test the tile, not to drive the VPN.

## Security note

The receiver is exported — the sender is another app, `com.android.shell` — and guarded by
`android.permission.DUMP`, which the shell holds and an ordinary app cannot obtain (it is
`signature|privileged|development`, so granting it to a third-party app itself takes adb). The guard
keeps other apps out; it was never meant to keep out someone with a cable, who can tap the tile with
`input` anyway.
