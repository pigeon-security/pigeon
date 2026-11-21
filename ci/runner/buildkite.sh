#!/bin/sh
# pigeon's CI bootstrap for a Buildkite step. The platform serves it at /ci/buildkite.sh; the
# pipeline's step runs it with Buildkite's checkout skipped. The build's env carries the job
# (PIGEON_*), never a secret: the step's OIDC token, which Buildkite issues for this job only, is
# exchanged at /ci/token for the job's token, which can fetch this one commit and report for
# this one job. Then the runner (/ci/runner.sh, the same one Containers runs) does the rest.
set -eu

for name in PIGEON_JOB PIGEON_TOKEN_URL PIGEON_RUNNER_URL; do
    eval "v=\${$name:-}"
    [ -n "$v" ] || { echo "pigeon: $name is not set" >&2; exit 2; }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Buildkite redacts the OIDC token from the log by default.
oidc=$(buildkite-agent oidc request-token --audience "$PIGEON_TOKEN_URL" --lifetime 300)

# The exchange: the OIDC token goes to curl on stdin, never on its command line.
printf '{"job":"%s"}' "$PIGEON_JOB" > "$work/exchange.json"
token=$(curl --config - -fsS --retry 3 --retry-all-errors --max-time 30 \
    -X POST -H 'Content-Type: application/json' \
    --data-binary "@$work/exchange.json" "$PIGEON_TOKEN_URL" <<CONF
header = "Authorization: Bearer $oidc"
CONF
)
unset oidc
case $token in
    pgn_ci_*) ;;
    *) echo "pigeon: the token exchange answered no job token" >&2; exit 3 ;;
esac
# Keep it out of the build log should anything print it.
printf '%s' "$token" | buildkite-agent redactor add >/dev/null 2>&1 || true

runner_script_url="${PIGEON_RUNNER_URL%%/ci/runner/*}/ci/runner.sh"
curl -fsSL --retry 3 --max-time 60 -o "$work/runner.sh" "$runner_script_url"

PIGEON_JOB_TOKEN=$token PIGEON_WORK="$work/src" sh "$work/runner.sh"
