# The ingest runner image (docs/plans/ingest.md, decision 13.5).
#
# Build from the repository root:
#   docker build -f src/ingest-runner/Dockerfile .
# Or, as cloud-init does, from the public repository at an exact commit:
#   docker build -f src/ingest-runner/Dockerfile https://github.com/pigeon-security/pigeon.git#<commit>
# Add `--build-arg PIGEON_COMMIT=<commit>` to have `--version` name the commit.
#
# Nothing is pushed to a registry: the image is built where it runs.
#
# It reads only its environment:
#   INGEST_JOB    the job (JobSpec JSON)
#   SOURCE_KEYS   ACCESS_KEY_ID:SECRET for an S3 source (not needed for a local folder)
#   DEST_KEYS     ACCESS_KEY_ID:SECRET for the R2 bucket
#   SPOOL_DIR     where files wait (default /spool)
#   SPOOL_BYTES   the spool budget in bytes (default 50 GiB)
# It exits 0 when done, 3 when stopped by SIGTERM or SIGINT, 1 on an error.

FROM rust:1-bookworm AS build
ARG PIGEON_COMMIT=unknown
ENV PIGEON_COMMIT=${PIGEON_COMMIT}
WORKDIR /src
COPY . .
# .cargo/config.toml defaults to wasm32, so the native target is named.
RUN rustup target add x86_64-unknown-linux-gnu \
    && cargo build --release --locked -p pigeon-ingest-runner --target x86_64-unknown-linux-gnu

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home ingest \
    && mkdir /spool \
    && chown ingest:ingest /spool
COPY --from=build /src/target/x86_64-unknown-linux-gnu/release/pigeon-ingest-runner /usr/local/bin/pigeon-ingest-runner
USER ingest
ENV SPOOL_DIR=/spool
ENTRYPOINT ["/usr/local/bin/pigeon-ingest-runner"]
