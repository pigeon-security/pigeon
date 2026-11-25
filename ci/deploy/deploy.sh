#!/bin/sh
# The `deploy` job: deploys a platform Worker from the commit a landing moved main to: core
# (src/platform), or the Worker in the directory PIGEON_WORKER_DIR names, as `deploy-sites` sets
# it (docs/plans/workers-split.md). Before
# anything else, the job asks the proxy (`/cf/grant`) whether its token is a landing's deploy
# grant and stops at once when it is not, so a deploy submitted by hand fails in seconds rather
# than after the build. It then runs the allowlist check against the stand-in (which builds the
# Worker, once), so a deploy that would make a call the proxy refuses stops before anything changes;
# then the pinned wrangler deploys that same build through the Cloudflare API proxy with this
# job's own token, using the build-free config copy the allowlist check left
# (`.wrangler.built.toml`). The build is not run again: before the real deploy, a hash of build/
# is checked against the one taken after the build, and a difference stops the job (exit 3),
# which the proxy takes only while this job is its landing's deploy (see
# src/capabilities/src/land.rs). No stored token.
#
# The proxy enforces the deployed platform's allowlist, not this commit's. When the commit adds
# an entry for a call wrangler makes after the upload, the first deploy uploads the new Worker
# and is then refused at that call; the deploy is made once more, through the new allowlist. A
# refused call before the upload cannot be fixed that way, and is reported. Cloudflare 10429
# on the pre-upload Workers service lookup gets one separate retry of the same build.
#
# Once deployed, it checks the apex serves this commit's architecture page (see `published`).
#
# Exit codes, which the Lander reads:
#   0  deployed.
#   3  stopped before the first real `wrangler deploy` began (the token, URL and grant checks, the
#      build, the allowlist check, the build hash check, node, reading wrangler.toml). Nothing on Cloudflare changed.
#   1  any failure once the real deploy has started, even a refusal before the upload. Wrangler's
#      own 3 is mapped to 1 too, so 3 never means anything else.
set -eu
# The one place the code is decided: before the start every failure is 3, after it never 3.
started=0
# Timing lines, `[timing] <stage> <seconds>s`; they never fail the script.
t0=$(date +%s)
tick=$t0
lap() {
    now=$(date +%s)
    printf '[timing] %s %ss\n' "$1" "$(( now - tick ))" || true
    tick=$now
}
log=
finish() {
    rc=$?
    [ -z "$log" ] || rm -f "$log"
    rm -f "$root/${PIGEON_WORKER_DIR:-src/platform}/.wrangler.built.toml" \
        "$root/${PIGEON_WORKER_DIR:-src/platform}/.wrangler.built.sha256"
    if [ "$rc" -ne 0 ]; then
        if [ "$started" = 0 ]; then rc=3; elif [ "$rc" = 3 ]; then rc=1; fi
    fi
    printf '[timing] total %ss\n' "$(( $(date +%s) - t0 ))" || true
    exit "$rc"
}
trap finish EXIT
root=$(cd "$(dirname "$0")/../.." && pwd)

[ -n "${PIGEON_JOB_TOKEN:-}" ] || {
    echo "the deploy job needs its job token: \`token = true\` for it in .pigeon/ci.toml" >&2
    exit 2
}
# The proxies' host, from the URL this job fetched its commit from.
case ${PIGEON_GIT_URL:-} in
    https://*/git/*) proxy=${PIGEON_GIT_URL%%/git/*} ;;
    *) echo "PIGEON_GIT_URL does not name the git proxy" >&2; exit 2 ;;
esac

# Pre-flight: is this job token the deploy grant of the landing deploying its commit?
grant_body=$(mktemp)
code=$(curl -sS --max-time 30 -o "$grant_body" -w '%{http_code}' \
    -H "Authorization: Bearer $PIGEON_JOB_TOKEN" "$proxy/cf/grant") || code=000
# A proxy deployed before `/cf/grant` existed refuses the path itself ("pigeon's proxy does not
# allow ..."), which it says only after the job token passed its deploy-grant check: the grant is
# held. The first landing that adds the route deploys through such a proxy.
if [ "$code" = 403 ] && grep -q "pigeon's proxy does not allow" "$grant_body"; then
    code=200
fi
if [ "$code" = 200 ]; then
    rm -f "$grant_body"
    echo "== deploy grant: held"
else
    if [ "$code" = 000 ]; then
        message="the proxy could not be reached"
    else
        message=$(grep -o '"message":"[^"]*"' "$grant_body" | head -n 1 \
            | sed 's/^"message":"//; s/"$//' || true)
        if [ -z "$message" ]; then
            message=$(head -c 300 "$grant_body" || true)
        fi
    fi
    rm -f "$grant_body"
    echo "the proxy refused this job's deploy grant (HTTP $code): $message" >&2
    echo "a deploy job deploys only as the deploy of a landing: land the branch (the land tool) instead of submitting the job by hand" >&2
    exit 1
fi

lap grant
PIGEON_KEEP_BUILT=1 "$root/ci/deploy/allowlist.sh"
tick=$(date +%s)
. "$root/ci/deploy/node.sh"
lap node

dir=$root/${PIGEON_WORKER_DIR:-src/platform}
account=$(sed -n 's/^account_id = "\(.*\)"$/\1/p' "$dir/wrangler.toml" | head -n 1)
script=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$dir/wrangler.toml" | head -n 1)
cd "$dir"
log=$(mktemp)

# What the stand-in saw must be what deploys: build/ must be as it was after the build.
now=$(cd build && find . -type f | LC_ALL=C sort | xargs sha256sum | sha256sum)
if [ ! -f .wrangler.built.toml ] || [ "$now" != "$(cat .wrangler.built.sha256)" ]; then
    echo "build/ changed since the allowlist check, or its config copy is missing; not deploying" >&2
    exit 1
fi

deploy() {
    printf '\n== wrangler deploy through %s (attempt %s)\n' "$proxy" "$1"
    started=1
    tick=$(date +%s)
    rc=0
    CLOUDFLARE_API_BASE_URL=$proxy/cf/client/v4 CLOUDFLARE_ACCOUNT_ID=$account \
        CLOUDFLARE_API_TOKEN=$PIGEON_JOB_TOKEN WRANGLER_SEND_METRICS=false \
        npx --no-install wrangler deploy -c .wrangler.built.toml --containers-rollout immediate >"$log" 2>&1 || rc=$?
    cat "$log"
    lap deploy
    return "$rc"
}

# Wrangler 4.166.0 reads the Worker service and migration tag before uploading. The
# proxy allows only GET on this exact path. A post-upload operation can also read it,
# so require the service diagnostic and its own 10429 note before the upload marker.
rate_limited_before_upload() {
    [ -n "$account" ] && [ -n "$script" ] || return 1
    awk -v service="/accounts/$account/workers/services/$script" '
        /^Uploaded / { uploaded = 1 }
        /A request to the Cloudflare API \(/ {
            reading = index($0, "(" service ") failed.") > 0
        }
        reading && /\[code: 10429\]/ { limited = 1 }
        END { exit !(limited && !uploaded) }
    ' "$log"
}

# The architecture page is part of the Worker: built from its own sources and wrangler.toml, it
# is served on the zone's apex as `/architecture.json`, naming the commit it was built from
# (PIGEON_COMMIT, set for every job). Check the live page is this deploy's, so the published
# architecture can never be older than the platform. One request a few times while the new
# version rolls out; no job of its own.
zone=$(sed -n 's/^ZONE = "\(.*\)"$/\1/p' "$root/src/platform/wrangler.toml" | head -n 1)
published() {
    printf '\n== the architecture page on %s names this commit\n' "$zone"
    seen=
    tick=$(date +%s)
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
        seen=$(curl -sS --max-time 15 "https://$zone/architecture.json" 2>/dev/null \
            | grep -o '"commit": *"[0-9a-f]*"' | head -n 1 | grep -o '[0-9a-f]\{40\}' || true)
        if [ "$seen" = "$PIGEON_COMMIT" ]; then
            echo "https://$zone/architecture.json is $PIGEON_COMMIT's"
            lap published
            return 0
        fi
        sleep 10
    done
    echo "https://$zone/architecture.json names commit '${seen:-none}', not $PIGEON_COMMIT:" >&2
    echo "the apex is not serving this deploy's architecture page" >&2
    lap published
    return 1
}

# Only core serves the architecture page.
check() {
    if [ "${PIGEON_WORKER_DIR:-src/platform}" = src/platform ]; then
        published
    fi
}

if deploy 1; then
    check
    exit
fi
# A Worker's first deploy: right after the upload creates the script, Cloudflare can still answer
# wrangler's next lookup of it with "This Worker does not exist" (10007). The upload went
# through, so deploying once more, a little later, finishes the job.
if grep -q "^Uploaded " "$log" && grep -q "code: 10007" "$log"; then
    echo "Cloudflare did not yet know the Worker its upload just created; deploying again" >&2
    sleep 30
    deploy 2
    check
    exit
fi
if ! grep -q "pigeon's proxy does not allow" "$log"; then
    if rate_limited_before_upload; then
        sample=$(od -An -N1 -tu1 /dev/urandom) || sample=0
        delay=$(( 30 + sample % 16 ))
        echo "Cloudflare rate limited the pre-upload Worker lookup; retrying in ${delay}s" >&2
        sleep "$delay"
        deploy 2
        check
        exit
    fi
    echo "the deploy failed; see above" >&2
    exit 1
fi
if ! grep -q "^Uploaded " "$log"; then
    echo "the running proxy refused a call wrangler makes before the upload: this commit's" >&2
    echo "allowlist cannot take effect through the proxy it changes. Deploy it once outside" >&2
    echo "the land flow (see docs/proxies.md), then land again." >&2
    exit 1
fi
echo "refused after the upload: the new Worker, with its new allowlist, is up; deploying again" >&2
sleep 30
deploy 2
check
