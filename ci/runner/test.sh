#!/bin/sh
# pigeon's checks, as the `test` job in .pigeon/ci.toml runs them and as every commit passes
# them before it lands: formatting, clippy with warnings denied (the default target is WASM,
# see .cargo/config.toml), the tests (natively: they run git and need no Workers runtime), and
# the platform Worker, which builds for WASM only.
set -eu
cd "$(dirname "$0")/../.."

step() {
    printf '\n== %s\n' "$*"
}

step cargo fmt --check
cargo fmt --all --check

step cargo clippy --workspace -D warnings
cargo clippy --workspace -- -D warnings

step cargo test --workspace --exclude pigeon-platform
cargo test --workspace --exclude pigeon-platform --target x86_64-unknown-linux-gnu

step cargo check -p pigeon-platform
cargo check -p pigeon-platform

step passed
