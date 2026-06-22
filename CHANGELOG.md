# Changelog

All notable changes to the Floppa VPN CLI will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2-cli-alpha] - 2026-06-22

### Added
- Systemd service management support including:
  - `floppa-cli service install` for systemd unit creation
  - `floppa-cli service status` without sudo password requirement
  - `floppa-cli service uninstall` for graceful removal
  - Enhanced sudo user handling and user context for service commands
  - Support for both system and user scope services
- Account login method selection alongside Telegram authentication
- Improved service command collision resolution

### Fixed
- Resolved `login-account` command collision with global `--log-file` argument
- Fixed sudo password prompts for service status checks
- Improved user handling when `USER=root` environment variable is set
- Enhanced log file path handling and permissions
- Enhanced peer management lifecycle commands
- Improved device identity management

## [0.1.1-cli-alpha] - 2026-06-21

### Changed
- Migration from wrapper-based execution to direct `floppa-cli` binary usage
- Enhanced systemd integration with improved user context handling
- Refined release workflow including checksum generation fixes
- Added comprehensive systemd service management capabilities

## [0.1.0-cli-alpha] - 2026-06-18

### Added
- Primary release with basic CLI functionality
- WireGuard/AmneziaWG tunnel support
- VLESS protocol support
- API client integration
- Authentication and device management

## [0.0.1-internal]

### Added
- Initial internal development version
- Core CLI structure and command framework
