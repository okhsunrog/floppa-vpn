# Endpoint resolution and native API transport

`floppa-net` is shared by the VPN actor, Rust API client and Tauri frontend transport.
It preserves hostnames for TLS verification; a DNS answer is only a candidate address.

## Discovery and caching

- IP literals need no lookup. Fresh cached DNS answers are returned without network I/O.
- Expired/missing DNS entries trigger system resolution immediately and Google/Cloudflare
  DoH after 650 ms. DoH uses pinned bootstrap IPs with normal hostname/certificate verification.
- Discovery is limited to four seconds and 16 addresses. Concurrent requests for one identity
  share discovery. Negative answers from one source do not suppress other sources.
- OS getaddrinfo does not expose TTL, so its results have a conservative 60-second lifetime.
  DoH TTL is respected, capped at an hour and including aliases. Zero-TTL answers are usable
  for the current discovery only.
- Up to four verified addresses survive restart for seven days. API cache identities include
  scheme/host/port; VPN identities include protocol, endpoint and a SHA-256 fingerprint of the
  server public key. Secret keys and bearer tokens are never persisted in this cache.
- Recent failures lower candidate priority for 60 seconds. A failed cached connection triggers
  fresh discovery. Platform network-change notifications invalidate DNS freshness and penalties.
- Cache writes are atomic, serialized within a process and best effort. Corrupt cache files are
  ignored. The cache lives beside application configuration (or in the platform cache directory
  for callers without application initialization).

For a DNS-free first API connection, a deployment can optionally set `FLOPPA_API_BOOTSTRAP`
when compiling Rust. The value is JSON mapping an origin with explicit port to candidate IPs:

```text
{"https://vpn.example:443":["192.0.2.10","192.0.2.11"]}
```

No production server address is baked in by default. Bootstrap addresses are never trusted
instead of TLS; maintained DNS and successfully verified addresses supersede their priority.
Changing these initial addresses requires a new build. Last-working addresses update at runtime.

## Connection attempts

The VPN actor attempts candidates under its existing overall attempt budget. A failed tunnel
verification unwinds its network changes before the next address is attempted; failed cleanup
stops retries. Android retries use a new service generation. Permission/configuration/platform
errors are not treated as reasons to cycle through addresses.

HTTP attempts have a 15-second header/connection budget and a three-second connect timeout.
Only connection failures can cause an automatic replay. An error after a request may have been
sent is returned to the caller, including for POST requests and one-time login-code exchanges.
Existing application-level retries are separate from transport retries.

Android selects a non-VPN Network in each process. DoH uses that network directly; API requests
first try the normal route (which may be a working VPN) and fall back to the underlay only on
connection failure. A private one-destination CONNECT relay binds
its upstream TCP socket using `android_setsocknetwork` before connect. Reqwest still performs TLS
end to end. The relay belongs to the response body and closes on completion, cancellation or its
25-second limit. It is not a WebView or system proxy. No process-wide network binding is used.

## Frontend and validation

The generated Hey API SDK receives `nativeFetch` in Tauri and browser fetch in the web build.
Pinia Colada and endpoint types remain unchanged. Native fetch preserves request bodies, HTTP
status, bearer/refresh headers and cancellation. It accepts only the configured API origin;
redirects are rejected. Request bodies are limited to 4 MiB and responses to 8 MiB. Streaming,
cookie-based authentication and arbitrary cross-origin fetches are not part of this adapter.
Update metadata uses the same transport; external browser navigations do not.

Hey API generates Zod response validators from the existing OpenAPI document. Additional fields
are accepted. Integer IDs are validated as numbers without coercion; RFC3339 date-time offsets
are accepted. Binary/text and empty responses retain the generated SDK's handling. Invalid
successful JSON responses are rejected before they reach Colada. Server-side semantic validation
of VPN keys, routes and permissions is still required.

Telegram continues to open the external browser and return through `floppa://auth`. The final
code exchange uses the native transport. Browser DNS and browser access to the login page remain
outside this mechanism.

## Verification

Local tests exercise cache expiry/restart, identity isolation, connection failover, late address
arrival, non-replay after a lost POST response, certificate rejection, frontend auth headers,
HTTP failures, cancellation before send, empty responses and generated validation.

Read-only live discovery:

```sh
cargo run -p floppa-net --example resolve -- floppa.okhsunrog.dev
```

On 2026-10-12, release APK 0.6.4 from this branch was installed on a Pixel 8 Pro over 0.6.3,
preserving application data. The user confirmed that the app works and Telegram login succeeds.

Compiler, unit-test and basic device success do not establish availability on a filtered ISP.
Device checks still need real Android roaming/lockdown and desktop reconnect after network loss.
DoH can itself be blocked; cached/bootstrap IPs cannot overcome blocking of every server address.
