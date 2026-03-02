# Floppa VPN build and deployment helpers

# Default target architecture for VPS deployment
target := "x86_64-unknown-linux-gnu"
release_dir := "release"

# Build all binaries in release mode (frontend is embedded in floppa-server via memory-serve)
build: build-frontend
    cargo build --release -p floppa-daemon -p floppa-server

# Build for specific target (cross-compilation)
build-target:
    cargo build --release --target {{target}} -p floppa-daemon -p floppa-server

# Create deployment archive with binaries, migrations, and systemd units
package: build
    #!/usr/bin/env bash
    set -euo pipefail

    rm -rf {{release_dir}}
    mkdir -p {{release_dir}}/{bin,migrations,systemd}

    # Copy binaries
    cp target/release/floppa-daemon {{release_dir}}/bin/
    cp target/release/floppa-server {{release_dir}}/bin/

    # Copy migrations
    cp -r migrations/* {{release_dir}}/migrations/

    # Copy config example
    cp config.example.toml {{release_dir}}/

    # Create systemd service files
    cat > {{release_dir}}/systemd/floppa-daemon.service << 'EOF'
    [Unit]
    Description=Floppa VPN WireGuard Daemon
    After=network-online.target postgresql.service
    Wants=network-online.target
    Requires=postgresql.service
    StartLimitIntervalSec=60
    StartLimitBurst=5

    [Service]
    Type=simple
    WorkingDirectory=/opt/floppa-vpn
    ExecStart=/opt/floppa-vpn/bin/floppa-daemon
    Environment=FLOPPA_CONFIG=/etc/floppa-vpn/config.toml
    Environment=FLOPPA_SECRETS=/etc/floppa-vpn/secrets.toml
    Restart=on-failure
    RestartSec=5

    [Install]
    WantedBy=multi-user.target
    EOF

    cat > {{release_dir}}/systemd/floppa-server.service << 'EOF'
    [Unit]
    Description=Floppa VPN Server (Bot + Admin API)
    After=network-online.target postgresql.service
    Wants=network-online.target
    StartLimitIntervalSec=60
    StartLimitBurst=5

    [Service]
    Type=simple
    User=floppa
    WorkingDirectory=/opt/floppa-vpn
    ExecStart=/opt/floppa-vpn/bin/floppa-server
    Environment=FLOPPA_CONFIG=/etc/floppa-vpn/config.toml
    Environment=FLOPPA_SECRETS=/etc/floppa-vpn/secrets.toml
    Restart=on-failure
    RestartSec=5

    [Install]
    WantedBy=multi-user.target
    EOF

    # Create archive
    tar -czvf floppa-vpn-release.tar.gz -C {{release_dir}} .

    echo "Created floppa-vpn-release.tar.gz"
    echo "Contents:"
    tar -tzvf floppa-vpn-release.tar.gz

# Cross-compile and package for target
package-target: build-target
    #!/usr/bin/env bash
    set -euo pipefail

    rm -rf {{release_dir}}
    mkdir -p {{release_dir}}/{bin,migrations,systemd}

    # Copy binaries from target directory
    cp target/{{target}}/release/floppa-daemon {{release_dir}}/bin/
    cp target/{{target}}/release/floppa-server {{release_dir}}/bin/

    # Copy migrations
    cp -r migrations/* {{release_dir}}/migrations/

    # Copy config example
    cp config.example.toml {{release_dir}}/

    # Create systemd service files (same as package)
    cat > {{release_dir}}/systemd/floppa-daemon.service << 'EOF'
    [Unit]
    Description=Floppa VPN WireGuard Daemon
    After=network-online.target postgresql.service
    Wants=network-online.target
    Requires=postgresql.service
    StartLimitIntervalSec=60
    StartLimitBurst=5

    [Service]
    Type=simple
    WorkingDirectory=/opt/floppa-vpn
    ExecStart=/opt/floppa-vpn/bin/floppa-daemon
    Environment=FLOPPA_CONFIG=/etc/floppa-vpn/config.toml
    Environment=FLOPPA_SECRETS=/etc/floppa-vpn/secrets.toml
    Restart=on-failure
    RestartSec=5

    [Install]
    WantedBy=multi-user.target
    EOF

    cat > {{release_dir}}/systemd/floppa-server.service << 'EOF'
    [Unit]
    Description=Floppa VPN Server (Bot + Admin API)
    After=network-online.target postgresql.service
    Wants=network-online.target
    StartLimitIntervalSec=60
    StartLimitBurst=5

    [Service]
    Type=simple
    User=floppa
    WorkingDirectory=/opt/floppa-vpn
    ExecStart=/opt/floppa-vpn/bin/floppa-server
    Environment=FLOPPA_CONFIG=/etc/floppa-vpn/config.toml
    Environment=FLOPPA_SECRETS=/etc/floppa-vpn/secrets.toml
    Restart=on-failure
    RestartSec=5

    [Install]
    WantedBy=multi-user.target
    EOF

    # Create archive
    tar -czvf floppa-vpn-release.tar.gz -C {{release_dir}} .

    echo "Created floppa-vpn-release.tar.gz"

# Run all checks (fmt, clippy, tests, frontend type-check + lint)
check:
    cargo fmt --check
    cargo clippy -- -D warnings
    cargo test
    cd floppa-web-shared && bun run type-check && bun run lint
    cd floppa-face && bun run type-check && bun run lint
    cd floppa-client && bun run type-check && bun run lint

# Format code
fmt:
    cargo fmt
    cd floppa-web-shared && bun run lint
    cd floppa-face && bun run lint
    cd floppa-client && bun run lint

# Lint
lint:
    cargo clippy -- -D warnings
    cd floppa-web-shared && bun run lint
    cd floppa-face && bun run lint
    cd floppa-client && bun run lint

# Clean build artifacts
clean:
    cargo clean
    rm -rf {{release_dir}} floppa-vpn-release.tar.gz

# Build frontend
build-frontend:
    cd floppa-face && bun install && bun run build

# Regenerate OpenAPI TypeScript client (no running backend needed)
openapi:
    cargo run -p floppa-server -- --openapi > floppa-web-shared/openapi.json
    cd floppa-web-shared && bun run openapi-ts

# Build Android APK (release, aarch64)
build-android:
    cd floppa-client && bun tauri android build --apk --target aarch64

# Build the gotatun test tunnel binary
build-test-tunnel:
    cargo build --release --manifest-path crates/floppa-test-tunnel/Cargo.toml

# Run VPN integration tests (requires Docker + tests/integration/.env)
test-integration: build-test-tunnel
    cd tests/integration && uv run pytest -v

# Deploy to Moscow VPS via Ansible (builds, packages, then deploys)
deploy: package
    cd ../cloud-forge && ansible-playbook site-moscow.yml --tags floppa
