#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(git rev-parse --show-toplevel)
git_dir=$(git rev-parse --path-format=absolute --git-common-dir)
cargo_dir=${CARGO_HOME:-$HOME/.cargo}
rustup_dir=${RUSTUP_HOME:-$HOME/.rustup}
build_dir=${FLOPPA_SERVER_BUILD_DIR:-$repo_dir/target/musl-server}
rustup target add x86_64-unknown-linux-musl
mode=build
if [[ ${1:-} == --test ]]; then
    mode=test
    shift
    : "${DATABASE_URL:?Set DATABASE_URL to an isolated test PostgreSQL instance}"
fi
if [[ ${1:-} == --tls-smoke ]]; then
    mode=tls
    shift
fi
packages=("$@")
if [[ $# -eq 0 ]]; then
    packages=(floppa-daemon floppa-server)
fi
for package in "${packages[@]}"; do
    case "$package" in
        floppa-daemon|floppa-server|floppa-vless) ;;
        floppa-core)
            if [[ $mode != test ]]; then
                echo "floppa-core is supported only in test mode" >&2
                exit 1
            fi
            ;;
        *) echo "Unsupported server package: $package" >&2; exit 1 ;;
    esac
done
mkdir -p "$build_dir"
build_dir=$(realpath "$build_dir")

# Build context is only this Dockerfile; no configuration or credentials are sent.
docker build -t floppa-server-build:musl - < "$repo_dir/scripts/Dockerfile.server-build"
docker_args=()
if [[ $mode == test ]]; then
    docker_args=(--network host --env DATABASE_URL)
fi
docker run --rm --init "${docker_args[@]}" -e BUILD_MODE="$mode" \
    -e RUSTUP_HOME=/rustup -e CARGO_HOME=/cargo -e SQLX_OFFLINE=true \
    -e CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
    -e CC_x86_64_unknown_linux_musl=musl-gcc \
    -v "$rustup_dir:/rustup:ro" -v "$cargo_dir/bin:/rust-bin:ro" \
    -v "$cargo_dir/registry:/cargo/registry" -v "$cargo_dir/git:/cargo/git" \
    -v "$repo_dir:$repo_dir" -v "$git_dir:$git_dir:ro" -v "$build_dir:/build" \
    -w "$repo_dir" floppa-server-build:musl bash -c '
        set -euo pipefail
        git config --global --add safe.directory "$PWD"
        export PATH=/rust-bin:$PATH
        args=()
        for package in "$@"; do args+=(-p "$package"); done
        if [[ $BUILD_MODE == test ]]; then
            cargo test --locked --release --target x86_64-unknown-linux-musl --target-dir /build/target "${args[@]}"
            exit
        fi
        if [[ $BUILD_MODE == tls ]]; then
            cargo test --locked --release --target x86_64-unknown-linux-musl --target-dir /build/target "${args[@]}" telegram_https_uses_native_roots -- --ignored
            exit
        fi
        cargo build --locked --release --target x86_64-unknown-linux-musl --target-dir /build/target "${args[@]}"
        for package in "$@"; do
            binary=/build/target/x86_64-unknown-linux-musl/release/$package
            # A static ELF must have neither a loader nor shared-library dependencies.
            headers=$(readelf -l "$binary")
            dynamic=$(readelf -d "$binary")
            if [[ $headers == *INTERP* || $dynamic == *NEEDED* ]]; then
                echo "Dynamic dependency found in $package" >&2
                exit 1
            fi
            file "$binary"
            if [[ $package == floppa-server ]]; then
                "$binary" --openapi > /build/openapi.json
            fi
        done
    ' build-musl "${packages[@]}"
