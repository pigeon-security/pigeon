#!/bin/sh
# pigeon's checks, as the `test` job in .pigeon/ci.toml runs them and as every commit passes
# them before it lands: formatting, clippy with warnings denied (the default target is WASM,
# see .cargo/config.toml, and the smoke suite natively), the smoke suite
# (tests/smoke.rs and tests/smoke/, natively: it runs git and needs no Workers runtime) within
# 60 seconds, `.claude/settings.json` exactly what the policy generates, and the platform
# Worker, which builds for WASM only.
#
# The smoke suite is pigeon's only tests (policy rule `smoke-suite-fixed`): a `#[test]` anywhere
# else fails the gate.
set -eu
cd "$(dirname "$0")/../.."

NATIVE=x86_64-unknown-linux-gnu
SMOKE_LIMIT_SECS=60

# The same cargo settings for every step, so clippy, the test build and the run share artifacts.
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0

# The image's warm dependency cache, when its key matches this Cargo.lock; else a cold build.
. ./ci/runner/warm.sh
warm_target .

# Timing: `step <stage> <words>` prints `[timing] <stage> <seconds>s` for the stage that just
# finished, then starts the next one. `timing_end` closes the last stage and prints the total.
# These prints never fail the script.
script_start=$(date +%s)
stage=
stage_start=$script_start
stage_end() {
    if [ -n "$stage" ]; then
        printf '[timing] %s %ss\n' "$stage" "$(( $(date +%s) - stage_start ))" || true
    fi
    stage=
}
step() {
    stage_end
    stage=$1
    shift
    printf '\n== %s\n' "$*"
    stage_start=$(date +%s)
}
timing_end() {
    stage_end
    printf '[timing] total %ss\n' "$(( $(date +%s) - script_start ))" || true
}

step guard 'no tests outside the smoke suite (tests/smoke.rs and tests/smoke/)'
outside=$(grep -rln --include='*.rs' '#\[test\]' . --exclude-dir=target --exclude-dir=node_modules \
    | grep -v -e '^\./tests/smoke\.rs$' -e '^\./tests/smoke/.*\.rs$' || true)
if [ -n "$outside" ]; then
    printf 'a #[test] outside the smoke suite (tests/smoke.rs and tests/smoke/; policy rule smoke-suite-fixed):\n%s\n' "$outside" >&2
    exit 1
fi

step fmt cargo fmt --check
cargo fmt --all --check

step fetch cargo fetch --locked
cargo fetch --locked

step clippy cargo clippy --workspace -D warnings
cargo clippy --workspace -- -D warnings
cargo clippy -p pigeon-capabilities --test smoke --target "$NATIVE" -- -D warnings

step smoke-build the smoke suite, build
# Built first, so the limit holds the scenarios' running time, not the compile.
cargo test -p pigeon-capabilities --test smoke --target "$NATIVE" --no-run

step smoke-run the smoke suite, within ${SMOKE_LIMIT_SECS}s
start=$(date +%s)
cargo test -p pigeon-capabilities --test smoke --target "$NATIVE"
took=$(( $(date +%s) - start ))
printf 'the smoke suite took %ss\n' "$took"
if [ "$took" -gt "$SMOKE_LIMIT_SECS" ]; then
    printf 'the smoke suite took %ss, more than %ss\n' "$took" "$SMOKE_LIMIT_SECS" >&2
    exit 1
fi

step settings .claude/settings.json is what .pigeon/policy.toml generates
cargo run -q -p pigeon-capabilities --example settings --target "$NATIVE" -- --check

step check-platform cargo check -p pigeon-platform
cargo check -p pigeon-platform

stage_end
printf '\n== passed\n'
timing_end
