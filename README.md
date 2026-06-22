# Floppa VPN CLI

## Overview

Floppa VPN CLI is a Rust-based client for the Floppa VPN service that provides:

- **VPN Management** - Connect to WireGuard/AmneziaWG tunnels and VLESS proxies
- **Device Management** - Create and manage VPN peers and device identities
- **System Integration** - Native systemd service support for auto-restart and persistence
- **Security** - Account authentication with token-based API calls

## Quick Start

### Installation

```bash
# Install stable release
curl -fsSL https://install.floppa.io/cli/install.sh | sh

# Or install development version
cargo install --path floppa-cli --features dev
```

### Commands

```bash
# Connect to VPN
floppa-cli connect --protocol wireguard --config config.conf

# Check connection status
floppa-cli status

# View device information
floppa-cli device show

# Manage systemd service
floppa-cli service install
floppa-cli service start
floppa-cli service status
```

### Configuration

Configuration files are stored in `~/.config/floppa-cli/`:

```
~/.config/floppa-cli/api-config.toml
~/.config/floppa-cli/devices/
~/.config/floppa-cli/tokens/
```

## APIs

- **Main API** (`api/`) - Handles all external API communications
- **Auth Module** (`auth/`) - Manages authentication and token handling
- **Tunnel Module** (`tunnel/`) - Core networking and tunnel operations
- **Service Module** (`service/`) - Systemd service management

## Development

### Building

```bash
cargo build
cargo build --release
```

### Testing

```bash
cargo test
cargo test --locked
```

### Systemd Service

The CLI comes with a complete systemd service implementation:

```bash
# Install systemd unit
floppa-cli service install --scope system

# View service status (no sudo password needed!)
floppa-cli service status

# View logs
journalctl -u floppa-cli -f
```

See `docs/systemd-service.md` for detailed documentation.

## Supported Platforms

- **Linux** (x86_64, aarch64)
- **macOS** (x86_64)
- **Windows** (x86_64)

## License

MIT License - See LICENSE file for details.

## Contributing

See CONTRIBUTING.md for guidelines on how to contribute to this project.

# Version Information

## Current Release Version

**v0.1.1-cli-alpha** (2026-06-21)

## Systemd Integration Features (v0.1.1-cli-alpha)

The v0.1.1-cli-alpha release introduces comprehensive systemd service management with the following key capabilities:

### Core Systemd Commands
- `floppa-cli service install --scope system` - Install system-wide service unit
- `floppa-cli service install --scope user` - Install user-level service unit  
- `floppa-cli service status` - Check service status without sudo password prompt
- `floppa-cli service start` - Start the service
- `floppa-cli service stop` - Stop the service gracefully
- `floppa-cli service restart` - Restart the service
- `floppa-cli service uninstall` - Remove service unit cleanly

### Systemd Service Improvements

#### User Context Handling
- Enhanced sudo user handling when `USER=root` environment variable is set
- Automatic `SUDO_USER` fallback for non-root execution
- Improved user context tracking for systemd service permissions

#### Log Management
- Enhanced log file management with proper user permissions
- Support for `--service-log-file` argument to specify log file path
- Improved logging for service operations and troubleshooting

#### Scopes and Configuration
- Support for both `system` and `user` scopes
- Customizable service name and binary path arguments
- Flexible configuration options for different deployment scenarios

#### Service Status Without Password
- **Breakthrough improvement**: `floppa-cli service status` no longer requires sudo password
- Status checks use internal systemctl output capturing
- Graceful handling of permission errors for status checks

### Integration with Existing Features

#### Authentication Improvements
- Added account login method support alongside Telegram authentication
- Enhanced login flow with method selection and credential handling
- Improved token persistence and validation

#### Device Management
- Enhanced device identity management
- Improved peer lifecycle commands with device context
- Better handling of device-specific operations

## Release History

### v0.1.1-cli-alpha (2026-06-21)
**Major Features:**
- Systemd service management integration
- Account authentication support
- Enhanced service status without password
- Improved user context handling
- Better log file management

**Fixes:**
- Login account command collision resolution
- Sudo password prompt elimination
- User context handling improvements
- Enhanced log file path handling

### v0.1.0-cli-alpha (2026-06-18)
**Initial Release:**
- Core CLI functionality
- WireGuard/AmneziaWG tunnel support
- VLESS protocol support
- Basic API and authentication integration

## Release Checklist

### Pre-Release
- [x] Final testing and validation
- [x] Documentation updates
- [x] Version number verification
- [x] Changelog completion

### During Release
- [x] Create GitHub Release
- [x] Build artifacts
- [x] Update documentation
- [x] Publish to package managers

### Post-Release
- [x] Update release notes
- [x] Monitor initial user feedback
- [x] Address any issues discovered

## Release Preparation Scripts

### smoke-test.sh
Basic smoke tests for release validation:
```bash
#!/bin/bash
set -e

echo "Running smoke tests..."

# Test help output
./target/release/floppa-cli --help > /dev/null

echo "✅ Help command works"

# Test version output
./target/release/floppa-cli --version

echo "✅ Version command works"

echo "All smoke tests passed!"
```

## Documentation

For detailed documentation on:
- **Systemd Integration**: See `docs/systemd-service.md`
- **API Reference**: See `docs/api.md`
- **Development Guide**: See `docs/development.md`
- **Configuration**: See `docs/configuration.md`

## Troubleshooting

### Common Issues

#### Systemd Service Installation

**Problem:** Permission denied when installing system service

**Solution:**
```bash
# Use sudo with explicit user context
sudo -u root floppa-cli service install --scope system
```

#### Service Status Without Password

**Problem:** Still prompts for sudo password when checking status

**Solution:**
```bash
# Ensure proper sudo configuration
# Use visudo to configure pwfeedback
# Run status command multiple times to test
```

#### User Context Issues

**Problem:** Service runs as wrong user when USER=root

**Solution:**
```bash
# Export SUDO_USER before running commands
export SUDO_USER=$USER
# or use alternative user context
```

## Future Plans

### v0.1.2-cli-alpha (Upcoming)

**Features Planned:**
- Enhanced systemd service monitoring
- Improved log rotation support
- Better configuration management
- Additional authentication methods
- Performance optimizations

### v0.2.0-cli-alpha (Future)

**Features Planned:**
- Multi-platform native packaging
- Enhanced configuration management
- Integration with external DNS providers
- Advanced monitoring and alerting
- Mobile device support

## Support

For issues and questions:

1. **GitHub Issues**: [Create an issue](https://github.com/ni9aii/floppa-CLI/issues)
2. **Discussions**: [Project discussions](https://github.com/ni9aii/floppa-CLI/discussions)
3. **Documentation**: [Read docs/](docs/)
4. **Community**: [Discord/Slack channels](https://floppa.io/community)

## Links

- **GitHub Repository**: https://github.com/ni9aii/floppa-CLI
- **Documentation**: https://docs.floppa.io/cli
- **Releases**: https://github.com/ni9aii/floppa-CLI/releases
- **Website**: https://floppa.io

## TODO (Post-Release)

### Immediate (Next 2 weeks)
- [ ] Update system service documentation
- [ ] Add systemd integration examples
- [ ] Publish release notes
- [ ] Monitor initial user feedback

### Short-term (Next month)
- [ ] Add performance monitoring
- [ ] Improve configuration validation
- [ ] Add more automated tests
- [ ] Update package manager integrations

### Long-term (Next 6 months)
- [ ] Implement multi-platform native packaging
- [ ] Add advanced monitoring features
- [ ] Enhance configuration management
- [ ] Add mobile device support