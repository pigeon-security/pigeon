#!/bin/sh
# The `data-worker-build` job: the deploy job of the `data-worker` artifact lane (ci/workers.toml,
# pigeon_core::lanes::ARTIFACT_LANES). It builds the data Worker (src/data-worker) with
# worker-build and puts the build where instance plans read it (docs/plans/email.md,
# follow-up 2; instances::data_plan): each module in the platform account's bucket
# pigeon-data-artifacts at data-worker/{digest}/{module}, then data-worker/current.json,
# `{"digest", "main", "modules": [{"name", "content_type"}]}`, last, so a build half uploaded is
# never the current one. The digest is data_plan::artifact_digest's: the SHA-256 of the lines
# `{module} {sha256 of its bytes}`, in the manifest's order, joined by newlines. A module already
# there with the same bytes is not uploaded again, nor is a manifest already naming this build.
# It deploys no Worker: pigeon-data is uploaded to an instance's account by an instance plan.
#
# It runs only as a landing's deploy, like the Worker deploys: its job token reaches the deploy
# proxy (`/cf/`) only while its landing is deploying its commit, and the proxy lets this lane's
# grant make only these calls on this bucket (cf_proxy::artifact_call), each with a token of R2
# read or write alone. A missing bucket is created, by its name alone, on the first run.
#
# Exit codes, which the Lander reads:
#   0  the build is the current one.
#   3  stopped before writing current.json (the token, URL and grant checks, the bucket, the
#      build, the digest, the modules): what instance plans read did not change. The bucket may
#      have been created, and modules of a new build may be in it; no manifest names them.
#   1  any failure once the write of current.json has started.
set -eu
started=0
t0=$(date +%s)
tick=$t0
lap() {
    now=$(date +%s)
    printf '[timing] %s %ss\n' "$1" "$(( now - tick ))" || true
    tick=$now
}
work=
finish() {
    rc=$?
    [ -z "$work" ] || rm -rf "$work"
    if [ "$rc" -ne 0 ]; then
        if [ "$started" = 0 ]; then rc=3; elif [ "$rc" = 3 ]; then rc=1; fi
    fi
    printf '[timing] total %ss\n' "$(( $(date +%s) - t0 ))" || true
    exit "$rc"
}
trap finish EXIT
root=$(cd "$(dirname "$0")/../.." && pwd)

# The bucket and folder, as instances::data_plan names them.
BUCKET=pigeon-data-artifacts
FOLDER=data-worker
MAIN=index.js
WORKER_BUILD=0.9.0

[ -n "${PIGEON_JOB_TOKEN:-}" ] || {
    echo "the data-worker-build job needs its job token: \`token = true\` for it in .pigeon/ci.toml" >&2
    exit 2
}
case ${PIGEON_GIT_URL:-} in
    https://*/git/*) proxy=${PIGEON_GIT_URL%%/git/*} ;;
    *) echo "PIGEON_GIT_URL does not name the git proxy" >&2; exit 2 ;;
esac
# The platform's own account, which holds the bucket.
account=$(sed -n 's/^account_id = "\(.*\)"$/\1/p' "$root/src/platform/wrangler.toml" | head -n 1)
[ -n "$account" ] || { echo "src/platform/wrangler.toml names no account_id" >&2; exit 2; }
buckets=$proxy/cf/client/v4/accounts/$account/r2/buckets
api=$buckets/$BUCKET
work=$(mktemp -d)

# One call through the deploy proxy with this job's token: `call METHOD URL OUT [FILE TYPE]`
# writes the answer's body to OUT and prints its HTTP status (curl's 000 when unreachable).
# Cloudflare rate limits (429), its 5xx and an unreachable proxy are retried with the same call,
# up to five attempts in all, with jittered, doubling waits (about 30-45s, 60-90s, 120-180s,
# 240-360s), as deploy.sh does for Workers; every call here is a read or an idempotent put.
call() {
    attempt=1
    base=30
    while :; do
        code=$(call_once "$@")
        case "$code" in
        429 | 5?? | 000) ;;
        *) echo "$code"; return ;;
        esac
        if [ "$attempt" -ge 5 ]; then echo "$code"; return; fi
        sample=$(od -An -N1 -tu1 /dev/urandom) || sample=0
        delay=$(( base + (base / 2) * sample / 255 ))
        echo "Cloudflare answered $code for $1 ${2##*/objects/}; attempt $((attempt + 1)) in ${delay}s" >&2
        sleep "$delay"
        attempt=$(( attempt + 1 ))
        base=$(( base * 2 ))
    done
}
call_once() {
    if [ $# -gt 3 ]; then
        curl -sS --max-time 300 -X "$1" -o "$3" -w '%{http_code}' \
            -H "Authorization: Bearer $PIGEON_JOB_TOKEN" -H "Content-Type: $5" \
            --data-binary "@$4" "$2" || true
    else
        curl -sS --max-time 300 -X "$1" -o "$3" -w '%{http_code}' \
            -H "Authorization: Bearer $PIGEON_JOB_TOKEN" "$2" || true
    fi
}
# Why an answer failed: Cloudflare's (or the proxy's) message, else the start of the body.
why() {
    m=$(grep -o '"message":"[^"]*"' "$1" 2>/dev/null | head -n 1 | sed 's/^"message":"//; s/"$//' || true)
    [ -n "$m" ] || m=$(head -c 300 "$1" 2>/dev/null || true)
    printf '%s' "$m"
}

# Pre-flight, as deploy.sh: is this job token the deploy grant of the landing deploying it?
code=$(call GET "$proxy/cf/grant" "$work/grant")
if [ "$code" != 200 ]; then
    echo "the proxy refused this job's deploy grant (HTTP $code): $(why "$work/grant")" >&2
    echo "a deploy job runs only as the deploy of a landing: land the branch (the land tool) instead of submitting the job by hand" >&2
    exit 1
fi
echo "== deploy grant: held"

# The bucket, in the platform's account: created on the first run, by its name alone.
code=$(call GET "$api" "$work/bucket")
case $code in
    200) ;;
    404)
        printf '{"name":"%s"}' "$BUCKET" > "$work/create"
        code=$(call POST "$buckets" "$work/created" "$work/create" application/json)
        case $code in
            2?? | 409) echo "bucket $BUCKET: created" ;;
            *)
                echo "creating bucket $BUCKET failed (HTTP $code): $(why "$work/created")" >&2
                echo "create it once by hand in the platform's account (wrangler r2 bucket create $BUCKET), then deploy again" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "reading bucket $BUCKET failed (HTTP $code): $(why "$work/bucket")" >&2
        exit 1
        ;;
esac
lap grant

# Build, as the Workers' wrangler.toml build commands do.
. "$root/ci/runner/warm.sh"
warm_target "$root"
export CARGO_INCREMENTAL=0
printf '\n== build: worker-build --release in src/data-worker\n'
cd "$root/src/data-worker"
rm -rf build
cargo install -q "worker-build@$WORKER_BUILD"
worker-build --release
lap build

# The modules, in the manifest's order: the main module (wasm-bindgen's JavaScript, which
# imports the WASM), then each WASM module by name. worker/shim.mjs only re-exports index.js.
[ -f "build/$MAIN" ] || { echo "the build has no $MAIN" >&2; exit 1; }
modules=$MAIN
for f in build/*.wasm; do
    [ -f "$f" ] || continue
    modules="$modules ${f#build/}"
done
[ "$modules" != "$MAIN" ] || { echo "the build has no WASM module" >&2; exit 1; }
type_of() {
    case $1 in
        *.wasm) printf 'application/wasm' ;;
        *) printf 'application/javascript+module' ;;
    esac
}
sha() { sha256sum "$1" | cut -d' ' -f1; }
nl='
'
lines=
listed=
for m in $modules; do
    lines="$lines${lines:+$nl}$m $(sha "build/$m")"
    listed="$listed${listed:+,}{\"name\":\"$m\",\"content_type\":\"$(type_of "$m")\"}"
done
# The lines joined by newlines, with none after the last, as artifact_digest joins them.
digest=$(printf '%s' "$lines" | sha256sum | cut -d' ' -f1)
manifest="{\"digest\":\"$digest\",\"main\":\"$MAIN\",\"modules\":[$listed]}"
printf 'build %s:\n%s\n' "$digest" "$lines"

# Each module, unless the same bytes are there already.
for m in $modules; do
    key=$FOLDER/$digest/$m
    code=$(call GET "$api/objects/$key" "$work/have")
    if [ "$code" = 200 ] && [ "$(sha "$work/have")" = "$(sha "build/$m")" ]; then
        echo "$key: already there"
        continue
    fi
    case $code in
        200 | 404) ;;
        *) echo "reading $key failed (HTTP $code): $(why "$work/have")" >&2; exit 1 ;;
    esac
    code=$(call PUT "$api/objects/$key" "$work/put" "build/$m" "$(type_of "$m")")
    case $code in
        2??) echo "$key: uploaded" ;;
        *) echo "uploading $key failed (HTTP $code): $(why "$work/put")" >&2; exit 1 ;;
    esac
done
lap modules

# The manifest last, unless it names this build already.
current=$FOLDER/current.json
code=$(call GET "$api/objects/$current" "$work/current")
if [ "$code" = 200 ] && [ "$(tr -d ' \n' < "$work/current")" = "$manifest" ]; then
    echo "$current: names $digest already"
    lap current
    exit 0
fi
case $code in
    200 | 404) ;;
    *) echo "reading $current failed (HTTP $code): $(why "$work/current")" >&2; exit 1 ;;
esac
printf '%s' "$manifest" > "$work/manifest"
started=1
code=$(call PUT "$api/objects/$current" "$work/put" "$work/manifest" application/json)
case $code in
    2??) echo "$current: now names $digest" ;;
    *) echo "writing $current failed (HTTP $code): $(why "$work/put")" >&2; exit 1 ;;
esac
lap current
