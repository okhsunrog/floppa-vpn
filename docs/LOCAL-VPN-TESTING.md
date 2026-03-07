# Local VPN Testing with Network Namespace

Test floppa-cli (gotatun/VLESS) through the real VPN path using a Linux network namespace for isolation. This avoids conflicting with existing WireGuard tunnels on the host.

## Architecture

### WireGuard path

```
[namespace floppa-test]          [host]                   [Moscow VPS]
  floppa-cli (gotatun)            veth-host (10.99.0.1)    wg-floppa
  floppa0 (TUN, 10.100.0.x)      │                         │
  veth-ns (10.99.0.2) ───────── veth-host ── NAT ──────── internet ── wg-floppa
```

Traffic path: namespace → veth pair → host NAT → internet → wg-floppa (Moscow) → wg1 (Moscow→Europe) → Europe VPS → internet.

### VLESS+REALITY path

```
[namespace floppa-test]          [host]                   [Europe VPS]
  floppa-cli (shoes-lite)         veth-host (10.99.0.1)    floppa-vless (REALITY)
  floppa0 (TUN, 10.0.0.2)        │                         │
  veth-ns (10.99.0.2) ───────── veth-host ── NAT ──────── internet ── floppa-vless
```

Traffic path: namespace → veth pair → host NAT → internet → HAProxy (EU, port 443) → floppa-vless → internet.

## Setup

All commands via `ssh root@localhost` (avoids sudo fingerprint prompts).

### 1. Create namespace and veth pair

```bash
ip netns add floppa-test
ip link add veth-host type veth peer name veth-ns
ip link set veth-ns netns floppa-test

ip addr add 10.99.0.1/24 dev veth-host
ip link set veth-host up

ip netns exec floppa-test ip addr add 10.99.0.2/24 dev veth-ns
ip netns exec floppa-test ip link set veth-ns up
ip netns exec floppa-test ip link set lo up
ip netns exec floppa-test ip route add default via 10.99.0.1
```

### 2. Enable NAT and forwarding

Replace `enp4s0f3u1u5` with your actual outbound interface.

```bash
sysctl -w net.ipv4.ip_forward=1
iptables -t nat -A POSTROUTING -s 10.99.0.0/24 -o enp4s0f3u1u5 -j MASQUERADE
iptables -A FORWARD -i veth-host -o enp4s0f3u1u5 -j ACCEPT
iptables -A FORWARD -i enp4s0f3u1u5 -o veth-host -m state --state RELATED,ESTABLISHED -j ACCEPT
```

### 3. Policy routing to bypass host VPN

If the host has its own WireGuard tunnel (e.g. wg0) that would intercept traffic, add a policy route so the namespace traffic uses the physical interface directly:

```bash
ip rule add from 10.99.0.0/24 table 200 priority 99
ip route add default via <gateway-ip> dev enp4s0f3u1u5 table 200
```

Find your gateway: `ip route show default` (the "via" address).

### 4. Copy auth token and connect

```bash
# Copy token so root can use it
mkdir -p /root/.config/floppa-cli
cp /home/<user>/.config/floppa-cli/token /root/.config/floppa-cli/token

# Connect (WireGuard)
ip netns exec floppa-test /path/to/floppa-cli connect

# Connect (VLESS)
ip netns exec floppa-test /path/to/floppa-cli connect --protocol vless --no-dns --interface floppa0
```

### 5. Run tests from the namespace

```bash
ip netns exec floppa-test curl -s ifconfig.me
ip netns exec floppa-test speedtest --simple
```

## Teardown

```bash
ip netns exec floppa-test ip link del floppa0 2>/dev/null
ip link del veth-host 2>/dev/null
ip netns del floppa-test 2>/dev/null
ip route del <endpoint-ip>/32 2>/dev/null
ip rule del table 200 2>/dev/null
ip route flush table 200 2>/dev/null
iptables -t nat -D POSTROUTING -s 10.99.0.0/24 -o enp4s0f3u1u5 -j MASQUERADE 2>/dev/null
iptables -D FORWARD -i veth-host -o enp4s0f3u1u5 -j ACCEPT 2>/dev/null
iptables -D FORWARD -i enp4s0f3u1u5 -o veth-host -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null
```

## Debugging VLESS+REALITY

### Client-side logging

floppa-cli supports `--log-file` for writing debug logs to a file:

```bash
RUST_LOG=debug ip netns exec floppa-test /path/to/floppa-cli \
  --log-file /tmp/floppa-cli.log \
  connect --protocol vless --no-dns --interface floppa0
```

Without `--log-file`, logs go to stderr. Use `RUST_LOG=shoes_lite=debug` for shoes-lite internals only.

### Server-side logging

Create a systemd override on the EU VPS to enable debug logging:

```bash
mkdir -p /etc/systemd/system/floppa-vless.service.d
cat > /etc/systemd/system/floppa-vless.service.d/debug.conf <<'EOF'
[Service]
Environment=RUST_LOG=debug,shoes_lite=debug
EOF
systemctl daemon-reload
systemctl restart floppa-vless
journalctl -u floppa-vless -f
```

Remove when done:

```bash
rm /etc/systemd/system/floppa-vless.service.d/debug.conf
rmdir /etc/systemd/system/floppa-vless.service.d
systemctl daemon-reload
systemctl restart floppa-vless
```

### shoes-lite release log level

shoes-lite uses the `log` crate with `release_max_level_info` — debug/trace logs are compiled out in release builds. To enable them temporarily, change in `shoes-lite/Cargo.toml`:

```toml
log = { version = "0.4", features = ["std", "release_max_level_debug"] }
```

Rebuild both floppa-cli and floppa-vless after changing this. Revert to `release_max_level_info` before committing.

### Deploying floppa-vless manually (without Ansible)

```bash
# Stop, deploy, start
ssh root@eu.okhsunrog.dev "systemctl stop floppa-vless"
scp target/release/floppa-vless root@eu.okhsunrog.dev:/opt/floppa-vless/bin/floppa-vless
ssh root@eu.okhsunrog.dev "systemctl start floppa-vless"
```

### Common issues

- **"Connection closed while reading VLESS response"**: Server blocked the outbound connection. Check that `ConnectRule` uses `vec![NetLocationMask::ANY]` (not `vec![]` — empty masks match nothing).
- **"ServerHello frame too short"**: The REALITY dest server rejected the ClientHello. Check signature_algorithms in `reality_tls13_messages.rs` includes RSA/ECDSA algorithms compatible with the dest's certificate.
- **No server-side logs after "Detected TLS ClientHello"**: The `process_stream` error is logged at `tracing::debug!` level in floppa-vless. Enable debug logging on the server.

## Changing speed limits for testing

Update the plan's speed limit directly in the database. The `plan_changed_trigger` notifies the daemon automatically:

```bash
ssh ubuntu@msk.okhsunrog.ru "sudo -u postgres psql floppa_vpn -c \"UPDATE plans SET default_speed_limit_mbps = 100 WHERE id = 5;\""
```

Set to `NULL` for unlimited. Use `test_plan` (id=5) for experiments.

## Reference results (March 2026)

### WireGuard — kernel module (baseline, host system)

| Download | Upload | Ping |
|----------|--------|------|
| ~394 Mbps | ~230 Mbps | ~53 ms |

### WireGuard — gotatun (userspace, namespace, MTU 1420)

| Speed Limit | Download | Upload |
|-------------|----------|--------|
| NULL (unlimited) | ~220-350 Mbps | ~60-135 Mbps |
| 100 Mbps | ~89 Mbps | ~19 Mbps |
| 50 Mbps | ~45 Mbps | ~23 Mbps |
| 20 Mbps | ~19 Mbps | ~17 Mbps |

### VLESS+REALITY — shoes-lite (userspace, namespace, MTU 1500)

| Build | Download | Upload | Ping |
|-------|----------|--------|------|
| Debug | ~117 Mbps | ~33 Mbps | ~107 ms |
| Release | ~384 Mbps | ~39 Mbps | ~94 ms |

Download matches kernel WG. Upload is limited by smoltcp userspace TCP stack overhead.
