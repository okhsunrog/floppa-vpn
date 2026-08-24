# iOS backend — implementation plan

Not implemented. This lived in the tree as `vpn/backend/ios.rs`: a full `impl VpnBackend`
whose every method returned `Err("IosBackend not yet implemented")`, never constructed by
`create_backend`, and compiled on every platform under `#![allow(dead_code)]`. It cost nothing
to run and something to keep — each change to the `VpnBackend` trait had to be mirrored into a
type nothing calls. The plan is worth keeping; the stub was not.

When iOS work actually starts, add the backend back as `#[cfg(target_os = "ios")]` so it is
compiled by the target that needs it rather than by all of them.

## Shape

The tunnel runs in a Network Extension (`NEPacketTunnelProvider`) — a separate process managed
by iOS, which survives the app closing. That is the same two-process shape as Android, with
Apple's IPC in place of tarpc over a Unix socket.

```text
UI Process (Tauri/WKWebView)     Network Extension (separate process)
┌───────────────────┐           ┌──────────────────────────────┐
│ IosBackend        │──Apple──→ │ NEPacketTunnelProvider       │
│                   │   IPC     │    └─ GotatunTunnel (Rust)   │
└───────────────────┘           └──────────────────────────────┘
```

- **UI → Extension**: `NETunnelProviderManager.sendProviderMessage(Data)`
- **Extension → UI**: `completionHandler(Data)` for responses
- **Shared data**: App Groups (UserDefaults / files) for config persistence
- **Serialization**: bincode, as on Android

Note the bincode constraint that Android already ran into: adjacently and internally tagged
enums cannot be *deserialized* from a non-self-describing format. Anything crossing this
boundary needs the externally tagged treatment `vpn::rpc::WireConfig` uses, and the tests in
`vpn/rpc.rs` pin exactly that.

The extension binary links a shared Rust static library containing `GotatunTunnel` and the
protocol types.

## Steps

1. Create an Xcode Network Extension target with an `NEPacketTunnelProvider` subclass.
2. Add the `com.apple.developer.networking.networkextension` entitlement — this requires
   membership of the Apple Developer Program.
3. Write a Swift `PacketTunnelProvider` that loads the Rust `.a` over C FFI:
   - `floppa_tunnel_handle_message(data, len) -> (data, len)` — handle a command, return a response
   - `startTunnel()`: read the config from the App Group, start gotatun on the `packetFlow` fd
   - `stopTunnel()`: stop gotatun
   - `handleAppMessage()`: deserialize, dispatch, return
4. Implement `IosBackend` against `NETunnelProviderManager`, either through the `objc2` crate or
   through thin Swift helpers exposed over C FFI. The latter is simpler.
5. Use App Groups to share config between the app and the extension.

The manager is what loads and saves the VPN configuration (`loadAllFromPreferences`), starts and
stops the tunnel (`connection.startVPNTunnel`), sends messages to the extension, and reports
status via `NEVPNConnection.status` through `NotificationCenter`.

## Connect flow

1. Write the config to App Group shared storage so the extension can read it.
2. Load or create an `NETunnelProviderManager`.
3. Configure its `NETunnelProviderProtocol`: `serverAddress` from the peer endpoint,
   `providerBundleIdentifier` set to the extension's bundle id.
4. `saveToPreferences()`, then `connection.startVPNTunnel()`.
5. The extension's `startTunnel(options:completionHandler:)` fires: it reads the config, creates
   `packetFlow`, and calls into Rust with the descriptor.
6. Watch `NEVPNConnection.status` for `.connected`.

## What the actor needs from it

`VpnBackend::observe` must distinguish "there is no tunnel" from "I could not reach the thing
that would know" — returning the second as the first is what previously made a transient IPC gap
read as a dropped tunnel. `NEVPNConnection.status` has a `.invalid` case that maps to
unreachable, not to disconnected.

`liveness_grace` should be non-zero, as on Android: the extension is a separate process and can
be restarted underneath the app.
