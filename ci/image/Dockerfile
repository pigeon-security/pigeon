# pigeon's CI base image, v4: what every Containers job runs in. Build it from the repository's
# root, so the runner script is in the context:
#
#   docker build --platform linux/amd64 -f ci/image/Dockerfile -t pigeon-ci-base:v4 .
#   npx wrangler containers push pigeon-ci-base:v4      # from src/platform, the pinned wrangler
#
# then put the pushed digest in src/platform/wrangler.toml under [containers.images.base].
#
# v2 (built by hand): debian bookworm-slim; git, curl, ca-certificates, coreutils,
# build-essential, pkg-config; Rust stable with wasm32-unknown-unknown, clippy and rustfmt; the
# runner as /usr/local/bin/pigeon-ci-runner.
# v4 adds a warm dependency layer: every dependency of the current Cargo.lock, built for WASM and
# natively into /opt/pigeon-target, with the key ci/runner/warm.sh checks in WARM_KEY. It is
# latent until the image is pushed and its digest pinned (see docs/plans/landing-speed.md).
# v3 adds what the `deploy` and `allowlist` jobs need: libssl-dev (worker-build links OpenSSL); Node.js 22 (wrangler 4.166.0 needs 22.12
# or later), checksum-checked; and worker-build 0.9.0, the version wrangler.toml's build command
# names, installed once rather than in every job. wrangler itself is not in the image: each job
# installs the commit's pinned one with `npm ci` in src/platform.
FROM debian:bookworm-slim

ARG NODE_VERSION=22.23.5
ARG NODE_SHA256=cdeb8670a0c81f51b4dc4ac61919f9b1ad917b304bf8221eb939570663d240f8
ARG WORKER_BUILD_VERSION=0.9.0

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/node/bin:$PATH

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git curl ca-certificates coreutils build-essential pkg-config xz-utils libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Rust stable, with the WASM target and the checks the `test` job runs.
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable \
    && rustup target add wasm32-unknown-unknown \
    && rustup component add clippy rustfmt \
    && chmod -R a+rwX "$RUSTUP_HOME" "$CARGO_HOME"

# Node.js, from the official tarball, checked against its published SHA-256.
RUN curl -fsSLo /tmp/node.tar.xz \
        "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" \
    && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
    && mkdir -p /usr/local/node \
    && tar -xJf /tmp/node.tar.xz -C /usr/local/node --strip-components=1 \
    && rm /tmp/node.tar.xz \
    && node --version && npm --version

# worker-build, which builds the platform Worker for WASM.
RUN cargo install --locked "worker-build@${WORKER_BUILD_VERSION}" \
    && rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git" \
    && chmod -R a+rwX "$CARGO_HOME"

# The warm dependency layer. Only manifests are copied, never repository source: each workspace
# member gets an empty lib.rs and main.rs (and a build.rs when it has build dependencies), so
# cargo builds the third-party dependencies and nothing of pigeon's. The builds are the ones
# ci/runner/test.sh and the deploy build run, with the same environment, so the artifacts match.
# Then pigeon's own stub artifacts are removed, so a real build never trusts them.
COPY Cargo.toml Cargo.lock rust-toolchain.toml /build/
COPY src /build/src-manifests
RUN cd /build \
    && for m in $(sed -n '/^members = \[/,/^\]/p' Cargo.toml | sed -n 's/^ *"\(.*\)",*$/\1/p'); do \
        mkdir -p "$m/src" \
        && cp "src-manifests/${m#src/}/Cargo.toml" "$m/Cargo.toml" \
        && : > "$m/src/lib.rs" \
        && echo 'fn main() {}' > "$m/src/main.rs" \
        && if grep -q '^\[build-dependencies\]' "$m/Cargo.toml"; then echo 'fn main() {}' > "$m/build.rs"; fi; \
    done \
    && rm -rf src-manifests \
    && mkdir -p tests src/capabilities/examples \
    && : > tests/smoke.rs \
    && echo 'fn main() {}' > src/capabilities/examples/stub.rs \
    && export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR=/opt/pigeon-target \
    && NATIVE=x86_64-unknown-linux-gnu \
    && cargo fetch --locked \
    && cargo clippy --workspace \
    && cargo clippy -p pigeon-capabilities --test smoke --target "$NATIVE" \
    && cargo test -p pigeon-capabilities --test smoke --target "$NATIVE" --no-run \
    && cargo build -p pigeon-capabilities --examples --target "$NATIVE" \
    && cargo check -p pigeon-platform \
    && for m in $(grep -l 'crate-type = \["cdylib"' src/*/Cargo.toml); do \
        cargo build --release -p "$(sed -n 's/^name = "\(.*\)"$/\1/p' "$m" | head -n 1)"; \
    done \
    && for m in src/*/Cargo.toml; do \
        cargo clean -p "$(sed -n 's/^name = "\(.*\)"$/\1/p' "$m" | head -n 1)" || true; \
    done \
    && printf '%s %s\n' "$(sha256sum Cargo.lock | cut -d' ' -f1)" "$(rustc -Vv | sha256sum | cut -d' ' -f1)" \
        > /opt/pigeon-target/WARM_KEY \
    && chmod -R a+rwX /opt/pigeon-target \
    && cd / && rm -rf /build

COPY ci/runner/entrypoint.sh /usr/local/bin/pigeon-ci-runner
RUN chmod 0755 /usr/local/bin/pigeon-ci-runner

ENTRYPOINT ["/usr/local/bin/pigeon-ci-runner"]
