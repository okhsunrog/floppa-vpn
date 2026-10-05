#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(git rev-parse --show-toplevel)
git_dir=$(git rev-parse --path-format=absolute --git-common-dir)
cargo_dir=${CARGO_HOME:-$HOME/.cargo}
rustup_dir=${RUSTUP_HOME:-$HOME/.rustup}
build_dir=${FLOPPA_SERVER_BUILD_DIR:-$repo_dir/target/ubuntu-server}
mkdir -p "$build_dir"
build_dir=$(realpath "$build_dir")

# Build context is only this Dockerfile; no configuration or credentials are sent.
docker build -t floppa-server-build:ubuntu24 - < "$repo_dir/scripts/Dockerfile.server-build"
docker run --rm --init \
    -e RUSTUP_HOME=/rustup -e CARGO_HOME=/cargo -e SQLX_OFFLINE=true \
    -v "$rustup_dir:/rustup:ro" -v "$cargo_dir/bin:/rust-bin:ro" \
    -v "$cargo_dir/registry:/cargo/registry" -v "$cargo_dir/git:/cargo/git" \
    -v "$repo_dir:$repo_dir" -v "$git_dir:$git_dir:ro" -v "$build_dir:/build" \
    -w "$repo_dir" floppa-server-build:ubuntu24 bash -c '
        set -euo pipefail
        git config --global --add safe.directory "$PWD"
        export PATH=/rust-bin:$PATH
        cargo build --locked --release --target-dir /build/target -p floppa-daemon -p floppa-server
        /build/target/release/floppa-server --openapi > /build/openapi.json
    '
