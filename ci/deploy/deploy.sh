#!/bin/sh
# The `deploy` job: deploys the platform Worker from the commit a landing moved main to. Before
# anything else, the job asks the proxy (`/cf/grant`) whether its token is a landing's deploy
# grant and stops at once when it is not, so a deploy submitted by hand fails in seconds rather
# than after the build. It then runs the allowlist check against the stand-in (which builds the
# Worker), so a deploy that would make a call the proxy refuses stops before anything changes;
# then the pinned wrangler deploys through the Cloudflare API proxy with this job's own token,
# which the proxy takes only while this job is its landing's deploy (see
# src/capabilities/src/land.rs). No stored token.
#
# The proxy enforces the deployed platform's allowlist, not this commit's. When the commit adds
# an entry for a call wrangler makes after the upload, the first deploy uploads the new Worker
# and is then refused at that call; the deploy is made once more, through the new allowlist. A
# refused call before the upload cannot be fixed that way, and is reported.
set -eu
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

"$root/ci/deploy/allowlist.sh"
. "$root/ci/deploy/node.sh"

account=$(sed -n 's/^account_id = "\(.*\)"$/\1/p' "$root/src/platform/wrangler.toml" | head -n 1)
cd "$root/src/platform"
log=$(mktemp)
trap 'rm -f "$log"' EXIT

deploy() {
    printf '\n== wrangler deploy through %s (attempt %s)\n' "$proxy" "$1"
    rc=0
    CLOUDFLARE_API_BASE_URL=$proxy/cf/client/v4 CLOUDFLARE_ACCOUNT_ID=$account \
        CLOUDFLARE_API_TOKEN=$PIGEON_JOB_TOKEN WRANGLER_SEND_METRICS=false \
        npx --no-install wrangler deploy --containers-rollout immediate >"$log" 2>&1 || rc=$?
    cat "$log"
    return "$rc"
}

deploy 1 && exit 0
if ! grep -q "pigeon's proxy does not allow" "$log"; then
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
