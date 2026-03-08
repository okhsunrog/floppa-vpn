# Third-Party VLESS Clients

Floppa VPN generates standard `vless://` URIs compatible with most VLESS/Xray clients.
Users copy the URI from the bot (`/vless`) or the web panel and paste it in their preferred app.

## Tested Clients (Android)

| App | Status | Import method | Links |
|-----|--------|---------------|-------|
| **v2rayNG** | Works | Clipboard (+ → Import from Clipboard) | [Play Store](https://play.google.com/store/apps/details?id=com.v2ray.ang) · [GitHub](https://github.com/2dust/v2rayNG/releases) |
| **NekoBox** | Works | Clipboard | [GitHub](https://github.com/MatsuriDayo/NekoBoxForAndroid/releases) (Play Store version is fake!) |
| **v2RayTun** | Works | Clipboard | [Play Store](https://play.google.com/store/apps/details?id=com.v2raytun.android) · [App Store](https://apps.apple.com/us/app/v2raytun/id6476628951) |
| **Hiddify** | Issues | Clipboard | [Play Store](https://play.google.com/store/apps/details?id=app.hiddify.com) · [GitHub](https://github.com/hiddify/hiddify-app/releases) |
| **Happ** | Issues | Clipboard | [Play Store](https://play.google.com/store/apps/details?id=com.happproxy) · [App Store](https://apps.apple.com/us/app/happ-proxy-utility/id6504287215) |

### Notes on non-working clients

- **Hiddify**: Connected on second attempt (first attempt showed an error). Internet didn't work through the tunnel. Needs investigation.
- **Happ**: Connected, but ping check showed "TLS handshake error". Traffic didn't flow. Likely a difference in xray core configuration (flow/fingerprint settings).

## iOS / iPadOS / macOS Clients (Not Yet Tested)

| App | Platforms | Price | Links |
|-----|-----------|-------|-------|
| **v2RayTun** | iOS, Android | Free | [App Store](https://apps.apple.com/us/app/v2raytun/id6476628951) |
| **Streisand** | iOS | Free | [App Store](https://apps.apple.com/us/app/streisand/id6450534064) |
| **FoXray** | iOS, macOS | Free | [App Store](https://apps.apple.com/us/app/foxray/id6448898396) |
| **Hiddify** | iOS, macOS, Android, Windows, Linux | Free | [App Store](https://apps.apple.com/us/app/hiddify-proxy-vpn/id6596777532) |
| **Happ** | iOS, macOS, tvOS, Android, Windows, Linux | Free | [App Store](https://apps.apple.com/us/app/happ-proxy-utility/id6504287215) |
| **Shadowrocket** | iOS | Paid | [App Store](https://apps.apple.com/us/app/shadowrocket/id932747118) |

## Deep Link / URI Handling

No tested Android app registers as a system handler for `vless://` URIs.
Clipboard import is the universal method across all clients.

Some apps have their own URI schemes:
- **v2RayTun**: `v2raytun://import/`
- **Hiddify**: `hiddify://import/<config-link>`
- **Happ**: `happ://crypto...` (encrypted subscriptions)

These are not used by Floppa VPN currently since clipboard import works everywhere.

## Windows / Linux Desktop Clients

| App | Platforms | Links |
|-----|-----------|-------|
| **v2rayN** | Windows | [GitHub](https://github.com/2dust/v2rayN/releases) |
| **NekoRay / NekoBox** | Windows, Linux | [GitHub](https://github.com/MatsuriDayo/nekoray/releases) |
| **Hiddify** | Windows, Linux, macOS | [GitHub](https://github.com/hiddify/hiddify-app/releases) |
