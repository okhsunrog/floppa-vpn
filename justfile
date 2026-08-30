# Floppa VPN build and deployment helpers

# Set up git hooks (run once after cloning)
setup:
    ln -sf ../../scripts/pre-commit .git/hooks/pre-commit

# Default target architecture for VPS deployment

target := "x86_64-unknown-linux-gnu"
release_dir := "release"

# Build all binaries in release mode (frontend is embedded in floppa-server via memory-serve)
build: build-frontend
    cargo build --release -p floppa-daemon -p floppa-server

# Build for specific target (cross-compilation)
build-target:
    cargo build --release --target {{ target }} -p floppa-daemon -p floppa-server

# Create deployment archive with binaries, migrations, and systemd units
package: build
    #!/usr/bin/env bash
    set -euo pipefail

    rm -rf {{ release_dir }}
    mkdir -p {{ release_dir }}/{bin,migrations,systemd}

    cp target/release/floppa-daemon {{ release_dir }}/bin/
    cp target/release/floppa-server {{ release_dir }}/bin/
    cp -r migrations/* {{ release_dir }}/migrations/
    cp config.example.toml {{ release_dir }}/
    cp systemd/*.service {{ release_dir }}/systemd/

    tar -czvf floppa-vpn-release.tar.gz -C {{ release_dir }} .

    echo "Created floppa-vpn-release.tar.gz"
    echo "Contents:"
    tar -tzvf floppa-vpn-release.tar.gz

# Cross-compile and package for target
package-target: build-target
    #!/usr/bin/env bash
    set -euo pipefail

    rm -rf {{ release_dir }}
    mkdir -p {{ release_dir }}/{bin,migrations,systemd}

    cp target/{{ target }}/release/floppa-daemon {{ release_dir }}/bin/
    cp target/{{ target }}/release/floppa-server {{ release_dir }}/bin/
    cp -r migrations/* {{ release_dir }}/migrations/
    cp config.example.toml {{ release_dir }}/
    cp systemd/*.service {{ release_dir }}/systemd/

    tar -czvf floppa-vpn-release.tar.gz -C {{ release_dir }} .

    echo "Created floppa-vpn-release.tar.gz"

# ktfmt (Kotlin formatter) — auto-downloaded on first use

ktfmt_version := "0.64"
ktfmt_jar := ".cache/ktfmt-" + ktfmt_version + "-with-dependencies.jar"
ktfmt_url := "https://repo1.maven.org/maven2/com/facebook/ktfmt/" + ktfmt_version + "/ktfmt-" + ktfmt_version + "-with-dependencies.jar"
kotlin_sources := "tauri-plugin-vpn/android/src"

[private]
ensure-ktfmt:
    @mkdir -p .cache
    @[ -f {{ ktfmt_jar }} ] || curl -sSL -o {{ ktfmt_jar }} {{ ktfmt_url }}

# Format Kotlin files
fmt-kotlin: ensure-ktfmt
    #!/usr/bin/env bash
    set -euo pipefail
    echo "ktfmt..."
    out=$(java -jar {{ ktfmt_jar }} --kotlinlang-style {{ kotlin_sources }} 2>&1) || { echo "$out"; exit 1; }

# Android Lint on the VPN plugin — the only linter that sees Android platform misuse
# (API-level guards, manifest correctness, policy-sensitive intents). ktfmt only formats.
#
# Not in ci.yml, and this is why: Gradle cannot configure the project until
# `gen/android/tauri.settings.gradle` exists. It carries absolute paths into the local cargo
# registry, so it is gitignored, and the only command that writes it is `tauri android build`.
# So the automated run lives in release.yml, right after the APK build that has already paid that
# cost. Use this recipe during development; `client-check` calls it.
lint-kotlin:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "android lint..."
    cd floppa-client/src-tauri/gen/android
    out=$(./gradlew :tauri-plugin-vpn:lintRelease --console=plain 2>&1) || { echo "$out"; exit 1; }

# Check Kotlin formatting
check-kotlin: ensure-ktfmt
    #!/usr/bin/env bash
    set -euo pipefail
    echo "ktfmt..."
    out=$(java -jar {{ ktfmt_jar }} --kotlinlang-style --set-exit-if-changed --dry-run {{ kotlin_sources }} 2>&1) || { echo "$out"; exit 1; }

# Run all checks (client + server). Use `just client-check` / `just server-check` to run one half.
check:
    @just --unstable --fmt --check
    @just client-check
    @just server-check

# Client: tauri Rust crates (fmt + clippy) + shared/client frontend + build + Android Kotlin
client-check:
    #!/usr/bin/env bash
    set -euo pipefail
    run() { local out; out=$("$@" 2>&1) || { echo "$out"; return 1; }; }
    echo "rustfmt (client crates)..."
    cargo fmt --check --manifest-path floppa-client/src-tauri/Cargo.toml
    cargo fmt --check --manifest-path tauri-plugin-vpn/Cargo.toml
    echo "clippy (client crates)..."
    cargo clippy --quiet --manifest-path floppa-client/src-tauri/Cargo.toml --all-targets -- -D warnings
    cargo clippy --quiet --manifest-path tauri-plugin-vpn/Cargo.toml --all-targets -- -D warnings
    # The Android branch of both crates is cfg-gated, so the host run above never compiles it.
    # The plugin gets its own run: as a path dependency of the client it is compiled but not linted.
    if command -v cargo-ndk >/dev/null; then
        echo "clippy (client crates, Android)..."
        cargo ndk -t arm64-v8a --manifest-path floppa-client/src-tauri/Cargo.toml clippy --quiet --all-targets -- -D warnings
        cargo ndk -t arm64-v8a --manifest-path tauri-plugin-vpn/Cargo.toml clippy --quiet --all-targets -- -D warnings
    else
        echo "clippy (client crates, Android): skipped, cargo-ndk is not installed"
    fi
    # The root workspace excludes these crates, so `just server-check`'s cargo test misses them.
    echo "tests (client crates)..."
    run cargo test --quiet --manifest-path floppa-client/src-tauri/Cargo.toml
    run cargo test --quiet --manifest-path tauri-plugin-vpn/Cargo.toml
    # `vp check` has no package filter, so it covers floppa-face too. That is cheaper than
    # arranging not to: the whole workspace takes under a second.
    echo "frontend: format + lint + types..."
    run vp check
    echo "frontend: vue type-check..."
    run vp run --filter floppa-web-shared --filter floppa-client typecheck
    echo "frontend: tests..."
    run vp test --run
    echo "floppa-client: build..."
    run vp run --filter floppa-client build
    just check-kotlin
    just lint-kotlin

# Server: workspace Rust (fmt + clippy + tests) + admin panel frontend (floppa-face)
server-check:
    #!/usr/bin/env bash
    set -euo pipefail
    run() { local out; out=$("$@" 2>&1) || { echo "$out"; return 1; }; }
    echo "rustfmt (workspace)..."
    cargo fmt --check
    echo "clippy (workspace)..."
    cargo clippy --quiet --workspace --all-targets -- -D warnings
    just machete
    echo "tests (workspace)..."
    output=$(cargo test --quiet 2>&1) || { echo "$output"; exit 1; }
    echo "frontend: format + lint + types..."
    run vp check
    echo "server frontend: vue type-check..."
    run vp run --filter floppa-face typecheck
    echo "floppa-face: build..."
    run vp run --filter floppa-face build

# Format all code (Rust + frontend + Kotlin)
fmt:
    #!/usr/bin/env bash
    set -euo pipefail
    run() { local out; out=$("$@" 2>&1) || { echo "$out"; return 1; }; }
    echo "rustfmt..."
    cargo fmt
    cargo fmt --manifest-path floppa-client/src-tauri/Cargo.toml
    cargo fmt --manifest-path tauri-plugin-vpn/Cargo.toml
    echo "frontend: format + lint..."
    run vp check --fix
    just fmt-kotlin

# Unused dependencies. One run at the root walks every Cargo.toml below it, so this covers the
# workspace as well as floppa-client/src-tauri and tauri-plugin-vpn, which the workspace excludes.
machete:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "cargo machete..."
    command -v cargo-machete >/dev/null || { echo "cargo-machete is not installed: cargo install cargo-machete"; exit 1; }
    out=$(cargo machete 2>&1) || { echo "$out"; exit 1; }

# Lint (without auto-fix): workspace + client crates + frontend
lint:
    @cargo clippy --quiet --workspace --all-targets -- -D warnings
    @cargo clippy --quiet --manifest-path floppa-client/src-tauri/Cargo.toml --all-targets -- -D warnings
    @cargo clippy --quiet --manifest-path tauri-plugin-vpn/Cargo.toml --all-targets -- -D warnings
    @vp check --no-fmt

# Prepare sqlx offline cache (requires running Postgres via DATABASE_URL)
sqlx-prepare:
    cargo sqlx prepare --workspace -- --all-targets

# Check sqlx cache matches the database (requires DATABASE_URL)
sqlx-check:
    cargo sqlx prepare --workspace --check -- --all-targets

# Clean build artifacts
clean:
    cargo clean
    rm -rf {{ release_dir }} release-vless floppa-vpn-release.tar.gz floppa-vless-release.tar.gz

# Build frontend
build-frontend:
    vp install --frozen-lockfile
    vp run --filter floppa-face build

# Regenerate the API clients — TypeScript and Rust — from the server's own annotations
openapi:
    cargo run -p floppa-server -- --openapi > floppa-web-shared/openapi.json
    cd floppa-web-shared && vp exec openapi-ts
    cargo run -p xtask -- api-types

# Client dev run with the MCP bridge, so an agent can drive the UI instead of aiming at pixels.
#
# Two things the ordinary dev run does not have, both off by default and both needed together:
# the `mcp-bridge` feature compiles the plugin in, and the config overlay turns on
# `withGlobalTauri`, which the bridge needs to reach the webview. `withGlobalTauri` lives in an
# overlay rather than the real config because it exposes `window.__TAURI__` to the page, and a
# release build has no reason to.
# Give the dev build the real icon in the task bar and window decoration.
#
# A packaged install gets this from the package. A dev run has no desktop entry at all, so both
# fall back to a generic icon; this writes the mapping by hand, pointing at the icon in the repo.
#
# The file name is not cosmetic. A window's icon is resolved two different ways on KDE/Wayland:
# the task bar matches `StartupWMClass` against the app id, while the window decoration looks for
# the desktop file whose *name* is the app id. Ours is `floppa-client` — the binary name, which is
# what GTK reports while `enableGtkAppId` is off — so the file has to be `floppa-client.desktop`
# or the title bar keeps the generic icon while the task bar shows the right one.
#
# `NoDisplay=true` keeps it out of the application menu: it is an icon mapping, not an install.
# Undo with `rm ~/.local/share/applications/floppa-client.desktop`.
[doc("Install a desktop entry so the dev build shows its own icon")]
dev-icon:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="$HOME/.local/share/applications"
    mkdir -p "$dir"
    cat > "$dir/floppa-client.desktop" <<EOF
    [Desktop Entry]
    Type=Application
    Name=Floppa VPN (dev)
    Comment=Development build of the Floppa VPN client
    Exec={{justfile_directory()}}/floppa-client/src-tauri/target/debug/floppa-client
    Icon={{justfile_directory()}}/floppa-client/src-tauri/icons/128x128.png
    StartupWMClass=floppa-client
    NoDisplay=true
    Terminal=false
    EOF
    sed -i 's/^    //' "$dir/floppa-client.desktop"
    update-desktop-database "$dir" 2>/dev/null || true
    echo "wrote $dir/floppa-client.desktop"

[doc("Client dev run with the MCP bridge (agent-drivable UI)")]
dev-mcp:
    cd floppa-client && vp exec tauri dev --features mcp-bridge --config src-tauri/tauri.mcp.conf.json

# Regenerate tauri-specta bindings (no running app needed)
bindings:
    cargo run --manifest-path floppa-client/src-tauri/Cargo.toml --bin export_bindings

android_apk := "floppa-client/src-tauri/gen/android/app/build/outputs/apk/arm64/release/app-arm64-release.apk"
android_pkg := "dev.okhsunrog.floppa_vpn"

# Optional: set ADB_DEVICE env var or pass device=SERIAL to target a specific device

adb_cmd := if env("ADB_DEVICE", "") != "" { "adb -s " + env("ADB_DEVICE", "") } else { "adb" }

# Build Android APK (release, aarch64)
build-android:
    cd floppa-client && vp exec tauri android build --target aarch64 --split-per-abi --apk

# Build and install Android APK on connected device
deploy-android device="": build-android
    {{ if device != "" { "adb -s " + device } else { adb_cmd } }} install -r {{ android_apk }}

# Start the Android app
app-start device="":
    {{ if device != "" { "adb -s " + device } else { adb_cmd } }} shell am start -n {{ android_pkg }}/.MainActivity

# Stop the Android app
app-stop device="":
    {{ if device != "" { "adb -s " + device } else { adb_cmd } }} shell am force-stop {{ android_pkg }}

# Restart the Android app
app-restart device="": (app-stop device) (app-start device)

# Show app logs (FloppaVPN tag, filtered by app PID)
app-logs device="":
    #!/usr/bin/env bash
    set -euo pipefail
    ADB="{{ if device != "" { "adb -s " + device } else { adb_cmd } }}"
    pid=$($ADB shell pidof {{ android_pkg }} 2>/dev/null || true)
    if [ -z "$pid" ]; then
        echo "App not running, showing recent logs..."
        $ADB logcat -d -s FloppaVPN | tail -50
    else
        echo "App PID: $pid"
        $ADB logcat -d --pid="$pid" -s FloppaVPN | tail -80
    fi

# Deploy, restart, and show logs
deploy-android-test device="": (deploy-android device) (app-restart device)
    #!/usr/bin/env bash
    set -euo pipefail
    ADB="{{ if device != "" { "adb -s " + device } else { adb_cmd } }}"
    sleep 3
    pid=$($ADB shell pidof {{ android_pkg }} 2>/dev/null || true)
    if [ -z "$pid" ]; then
        echo "App failed to start!"
        $ADB logcat -d -s FloppaVPN | tail -30
        exit 1
    fi
    echo "App PID: $pid"
    $ADB logcat -d --pid="$pid" | grep "FloppaVPN" | tail -50

# Run VPN integration tests (requires Docker + tests/integration/.env)
test-integration: build-cli
    cd tests/integration && uv run pytest -v

# Run speed limit integration tests (requires Docker, runs locally)
test-speed-limit:
    cd tests/integration && uv run pytest test_speed_limit.py -v -s

# Build floppa-cli
build-cli:
    cargo build --release -p floppa-cli

# Build floppa-vless binary in release mode
build-vless:
    cargo build --release -p floppa-vless

# Create deployment archive for floppa-vless (runs on the Moscow VPS behind HAProxy)
package-vless: build-vless
    #!/usr/bin/env bash
    set -euo pipefail

    rm -rf release-vless
    mkdir -p release-vless/{bin,systemd}

    cp target/release/floppa-vless release-vless/bin/
    cp systemd/floppa-vless.service release-vless/systemd/

    tar -czvf floppa-vless-release.tar.gz -C release-vless .

    echo "Created floppa-vless-release.tar.gz"
    echo "Contents:"
    tar -tzvf floppa-vless-release.tar.gz

# Deploy to Moscow VPS via Ansible (builds, packages, then deploys).
# Includes the network role so AmneziaWG's firewall port, NAT and tunnel routing are applied.
deploy: package
    cd ../cloud-forge && ansible-playbook site-moscow.yml --tags floppa,network

# Deploy to Europe VPS via Ansible (builds, packages, then deploys).
# Includes the network role so the AmneziaWG subnet gets exit NAT (masquerade). The floppa_vless
# role is disabled there (floppa-vless runs on Moscow — the `floppa` tag of `just deploy`), so this
# is mostly the network half; the archive is still built in case it is ever enabled.
deploy-europe: package-vless
    cd ../cloud-forge && ansible-playbook site-europe.yml --tags floppa-vless,network
