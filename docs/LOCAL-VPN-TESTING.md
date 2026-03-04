# Local VPN Testing with Network Namespace

Test floppa-cli (gotatun) through the real VPN path using a Linux network namespace for isolation. This avoids conflicting with existing WireGuard tunnels on the host.

## Architecture

```
[namespace floppa-test]          [host]                   [Moscow VPS]
  floppa-cli (gotatun)            veth-host (10.99.0.1)    wg-floppa
  floppa0 (TUN, 10.100.0.x)      │                         │
  veth-ns (10.99.0.2) ───────── veth-host ── NAT ──────── internet ── wg-floppa
```

Traffic path: namespace → veth pair → host NAT → internet → wg-floppa (Moscow) → wg1 (Moscow→Europe) → Europe VPS → internet.

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

# Connect
ip netns exec floppa-test /path/to/floppa-cli connect
```

### 5. Run tests from the namespace

```bash
ip netns exec floppa-test speedtest --simple
ip netns exec floppa-test curl -s ifconfig.me
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

## Changing speed limits for testing

Update the plan's speed limit directly in the database. The `plan_changed_trigger` notifies the daemon automatically:

```bash
ssh ubuntu@msk.okhsunrog.ru "sudo -u postgres psql floppa_vpn -c \"UPDATE plans SET default_speed_limit_mbps = 100 WHERE id = 5;\""
```

Set to `NULL` for unlimited. Use `test_plan` (id=5) for experiments.

## Reference results (gotatun, MTU 1420, March 2026)

| Speed Limit | Download | Upload |
|-------------|----------|--------|
| NULL (unlimited) | ~220-350 Mbps | ~60-135 Mbps |
| 100 Mbps | ~89 Mbps | ~19 Mbps |
| 50 Mbps | ~45 Mbps | ~23 Mbps |
| 20 Mbps | ~19 Mbps | ~17 Mbps |

Upload varies due to path bottleneck (double WG encapsulation through Europe exit node), not TC limits.
