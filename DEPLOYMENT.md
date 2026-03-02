# Floppa VPN Deployment Guide

## Prerequisites

- Ansible with vault configured (`~/.vault_pass`)
- `just` command runner
- `bun` for frontend build
- Rust toolchain
- Access to your VPS via SSH
- An Ansible repo with roles for PostgreSQL, nginx, and floppa-vpn (expected at `../cloud-forge/`)

## 1. Generate Credentials

### WireGuard Private Key
```bash
wg genkey
```
Save the output for `vault_floppa_wg_private_key`.

### Database Password
```bash
openssl rand -base64 24
```
Save the output for `vault_floppa_db_password`.

### JWT Secret
```bash
openssl rand -hex 32
```
Save the output for `vault_floppa_jwt_secret`.

### Encryption Key
```bash
openssl rand -hex 32
```
Save the output for `vault_floppa_encryption_key`.

### Telegram Bot Token
1. Open Telegram and message [@BotFather](https://t.me/BotFather)
2. Send `/newbot`
3. Choose a name (e.g., "Floppa VPN")
4. Choose a username (must end in `bot`, e.g., `FloppaVpnBot`)
5. Copy the token (format: `123456789:ABCdefGHI...`)

Save the token for `vault_floppa_bot_token`.

**Important:** Update `bot_username` in `cloud-forge/group_vars/moscow/vars.yml` to match your bot's username (without `@`).

### Admin Telegram ID
1. Open Telegram and message [@userinfobot](https://t.me/userinfobot)
2. It will reply with your user ID (a number like `123456789`)

Save the ID for `vault_floppa_admin_telegram_ids`.

## 2. Add Credentials to Vault

See `cloud-forge/group_vars/all/vault.yml.example` for the full template.

```bash
cd /path/to/cloud-forge
ansible-vault edit group_vars/all/vault.yml
```

Add the following section:

```yaml
# Floppa VPN
vault_floppa_wg_private_key: "YOUR_WG_PRIVATE_KEY"
vault_floppa_db_password: "YOUR_DB_PASSWORD"
vault_floppa_bot_token: "YOUR_BOT_TOKEN"
vault_floppa_jwt_secret: "YOUR_JWT_SECRET"
vault_floppa_encryption_key: "YOUR_ENCRYPTION_KEY"
vault_floppa_admin_telegram_ids:
  - 123456789  # Your Telegram ID
```

## 3. Build Release Package

```bash
cd /path/to/floppa-vpn
just package
```

This creates `floppa-vpn-release.tar.gz` containing:
- Compiled binaries (`floppa-daemon`, `floppa-bot`, `floppa-admin` with frontend embedded via memory-serve)
- Database migrations
- Systemd service files
- `config.example.toml`

The Vue frontend (`floppa-face`) is built first and embedded into the `floppa-admin` binary at compile time. No separate static files are deployed.

## 4. Deploy

```bash
cd /path/to/cloud-forge
ansible-playbook site-moscow.yml --tags floppa,nginx,network
```

Tags:
- `floppa` - Deploy the Floppa VPN service (PostgreSQL, binaries, config, systemd)
- `nginx` - Configure reverse proxy for your domain
- `network` - Open firewall port 51820/udp for WireGuard + NAT for Floppa subnet

The Ansible role expects the release archive at `../floppa-vpn/floppa-vpn-release.tar.gz` (relative to the cloud-forge directory).

### What the Ansible role does

1. Installs PostgreSQL, creates `floppa` DB user and `floppa_vpn` database
2. Creates `floppa:floppa` system user/group
3. Copies and extracts the release archive to `/opt/floppa-vpn/`
4. Generates WireGuard public key from vault private key
5. Renders `config.toml` (0644) and `secrets.toml` (0640) to `/etc/floppa-vpn/`
6. Installs and enables three systemd services
7. Database migrations run automatically on floppa-daemon startup

### Systemd services

- **floppa-daemon** - Runs as root (requires WireGuard + tc access). Syncs peers, tracks traffic, applies rate limits.
- **floppa-bot** - Runs as `floppa` user. Telegram bot for user self-service.
- **floppa-admin** - Runs as `floppa` user. REST API + embedded frontend on port 3000.

All services read `FLOPPA_CONFIG=/etc/floppa-vpn/config.toml` and `FLOPPA_SECRETS=/etc/floppa-vpn/secrets.toml`.

### Nginx

Nginx reverse-proxies `https://your-domain.example.com` → `localhost:3000` (floppa-admin serves both `/api/` routes and the embedded frontend). SSL via Let's Encrypt.

## 5. Verify Deployment

### Check services are running
```bash
ssh user@your-server "systemctl status floppa-daemon floppa-bot floppa-admin"
```

### Check logs
```bash
ssh user@your-server "journalctl -u floppa-daemon -f"
ssh user@your-server "journalctl -u floppa-bot -f"
ssh user@your-server "journalctl -u floppa-admin -f"
```

### Test the bot
Message your bot on Telegram - it should respond to `/start`.

### Access admin panel
Navigate to `https://your-domain.example.com` and log in with Telegram.

## Updating

After code changes:

```bash
# Rebuild package (builds frontend + Rust binaries, creates archive)
cd /path/to/floppa-vpn
just package

# Redeploy
cd /path/to/cloud-forge
ansible-playbook site-moscow.yml --tags floppa
```

Only the `floppa` tag is needed for updates (nginx and network rules don't change).

## Troubleshooting

### Bot not responding
- Check bot token is correct in vault
- Check `journalctl -u floppa-bot`
- Verify `bot_username` in `cloud-forge/group_vars/moscow/vars.yml` matches your bot's actual username

### WireGuard not working
- Check `journalctl -u floppa-daemon`
- Verify port 51820/udp is open: `sudo iptables -L -n | grep 51820`
- Check interface exists: `ip link show wg-floppa`
- Check NAT rule exists for 10.100.0.0/24

### Database errors
- Check PostgreSQL is running: `systemctl status postgresql`
- Check connection: `sudo -u postgres psql -d floppa_vpn -c '\dt'`

### Admin panel not loading
- Check `journalctl -u floppa-admin`
- Check nginx config: `sudo nginx -t`
- Verify SSL cert exists for the domain
