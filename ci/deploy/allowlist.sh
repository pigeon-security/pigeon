#!/bin/sh
# The allowlist check: a real `wrangler deploy` of src/platform, with the pinned wrangler, against
# a stand-in for the Cloudflare API (standin.mjs), once as for the Worker that exists and once as
# for a first deploy; then every call it made, with its JSON body, is checked against the Cloudflare
# API proxy's allowlist (`cf_proxy::unlisted`, through the `allowlist` example). Fails naming each
# call the proxy would refuse. Holds no credential: the stand-in takes a dummy token. Run from
# anywhere in the repository; the `allowlist` and `deploy` jobs run it.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
platform=$root/src/platform
toml=$platform/wrangler.toml
work=$(mktemp -d)
trap 'kill "$standin" 2>/dev/null || true; rm -rf "$work"' EXIT
standin=

# What the proxy's scope is made of, read from wrangler.toml.
value() { sed -n "s/^$1 = \"\(.*\)\"$/\1/p" "$toml" | head -n 1; }
account=$(value account_id)
script=$(value name)
migration=$(sed -n 's/^tag = "\(.*\)"$/\1/p' "$toml" | tail -n 1)
classes=$(sed -n 's/.*class_name = "\([A-Za-z0-9_]*\)".*/\1/p' "$toml" | sort -u | paste -sd, -)
queues=$(awk '/^\[\[queues\.producers\]\]/{p=1;next} /^\[/{p=0} p && /^queue = /{gsub(/"/,"",$3); print $3}' "$toml")
containers=$(awk '/^\[\[containers\]\]/{p=1;next} /^\[/{p=0} p && /^class_name = /{gsub(/"/,"",$3); print $3}' "$toml")
[ -n "$account" ] && [ -n "$script" ] || { echo "wrangler.toml names no account_id or name" >&2; exit 2; }

cd "$platform"
npm ci --no-audit --no-fund --loglevel=error

port=$((20000 + $$ % 20000))
for mode in existing fresh; do
    calls=$work/calls-$mode.txt
    : > "$calls"
    STANDIN_LOG=$calls STANDIN_PORT=$port STANDIN_MODE=$mode STANDIN_MIGRATION=$migration \
        STANDIN_CLASSES=$classes STANDIN_SCRIPT=$script node "$root/ci/deploy/standin.mjs" &
    standin=$!
    i=0
    until node -e "fetch('http://127.0.0.1:$port/').then(()=>process.exit(0),()=>process.exit(1))" 2>/dev/null; do
        i=$((i + 1))
        [ "$i" -lt 50 ] || { echo "the stand-in did not start" >&2; exit 2; }
        sleep 0.2
    done
    : > "$calls"
    printf '\n== wrangler deploy against the stand-in (%s)\n' "$mode"
    CLOUDFLARE_API_BASE_URL=http://127.0.0.1:$port/client/v4 \
        CLOUDFLARE_API_TOKEN=stand-in CLOUDFLARE_ACCOUNT_ID=$account \
        WRANGLER_SEND_METRICS=false \
        npx --no-install wrangler deploy --containers-rollout immediate
    kill "$standin"
    wait "$standin" 2>/dev/null || true
    standin=
done

printf '\n== the calls against the allowlist\n'
set -- --account "$account" --script "$script"
for q in $queues; do set -- "$@" --queue "$q"; done
for c in $containers; do
    set -- "$@" --namespace "$(node -e "console.log(require('crypto').createHash('sha256').update('$c').digest('hex').slice(0, 32))")"
done
cat "$work"/calls-*.txt > "$work/calls.txt"
sed 's/ body=.*/ (with a JSON body)/' "$work/calls.txt" | sort | uniq -c
cd "$root"
cargo run -q -p pigeon-capabilities --example allowlist --target x86_64-unknown-linux-gnu -- \
    "$work/calls.txt" "$@"
