# The AI agent sandbox image (docs/plans/ai-agents.md, decision 3): the CI base image, so what passes in
# a sandbox passes in CI, plus pigeon-sandboxd, the exec server an AI agent run's tools reach. Build
# it from the repository's root:
#
#   docker build --platform linux/amd64 -f ci/sandbox/Dockerfile -t pigeon-ai-agent-sandbox:v2 .
#   cd src/platform && npm ci && npx wrangler containers push pigeon-ai-agent-sandbox:v2
#
# then the pushed digest is pinned in src/ai-agents-worker/wrangler.toml under
# [containers.images.sandbox]. When the registry's base image cannot be pulled, build the CI base
# image locally first (ci/image/Dockerfile, tagged pigeon-ci-base:v3) and add
# `--build-arg BASE=pigeon-ci-base:v3`.
ARG BASE=registry.cloudflare.com/4f94676cb2cbe46fd985b089ca97ea27/pigeon-ci-base@sha256:02b7487373a845ae84c7ae631ba5c33b5b887d3d154ab2aba0f244ba2ac861a6
FROM ${BASE} AS build
COPY ci/sandbox/sandboxd /src/sandboxd
RUN cargo build --release --locked --manifest-path /src/sandboxd/Cargo.toml

FROM ${BASE}
COPY --from=build /src/sandboxd/target/release/pigeon-sandboxd /usr/local/bin/pigeon-sandboxd
RUN mkdir -p /work /tmp/out
WORKDIR /work
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/pigeon-sandboxd"]
