#!/bin/sh
# pigeon's CI runner: the entrypoint of a job container. The base image installs it as
# /usr/local/bin/pigeon-ci-runner, with git, curl, coreutils and sh beside it. The gateway starts
# one container per job with the PIGEON_* variables below. The runner fetches the one commit
# through the git proxy with the job token, reports `running`, runs the job's script without the
# token in its environment, and reports the result. An exit without a report is a lost runner,
# which the gateway retries elsewhere once.
#
# The token can only fetch this commit and report for this job, until the job's deadline. A job
# script could still read it from this shell's initial environment (/proc), which gives it
# nothing it does not already have: the commit, and its own exit code.
set -eu

# Fail before any report: the gateway treats this as a lost runner.
need() {
    eval "v=\${$1:-}"
    [ -n "$v" ] || exit 2
}
for name in PIGEON_JOB PIGEON_COMMIT PIGEON_SCRIPT PIGEON_TIMEOUT_MIN \
    PIGEON_GIT_URL PIGEON_RUNNER_URL PIGEON_JOB_TOKEN; do
    need "$name"
done

# The commit must be exactly 40 lowercase hex characters.
case $PIGEON_COMMIT in
    *[!0-9a-f]*) exit 2 ;;
esac
[ "${#PIGEON_COMMIT}" -eq 40 ] || exit 2

# The timeout must be digits only.
case $PIGEON_TIMEOUT_MIN in
    *[!0-9]*) exit 2 ;;
esac

# Timing lines, `[timing] <stage> <seconds>s`. They never fail the runner.
t0=$(date +%s)
tick=$t0
lap() {
    now=$(date +%s)
    printf '[timing] %s %ss\n' "$1" "$(( now - tick ))" || true
    tick=$now
}

# Where the commit is checked out: /work in a container, a temporary directory elsewhere.
work=${PIGEON_WORK:-/work}

# Keep the token in a plain shell variable (not exported).
token=$PIGEON_JOB_TOKEN
unset PIGEON_JOB_TOKEN v

body=$(mktemp)
out=$(mktemp)
trap 'rm -f "$body" "$out"' EXIT

# Post a JSON body. The token goes to curl through stdin, never argv.
report() {
    printf '%s' "$1" > "$body"
    curl --config - -fsS --retry 3 --retry-all-errors --max-time 30 \
        -X POST -H 'Content-Type: application/json' \
        --data-binary "@$body" -o /dev/null "$PIGEON_RUNNER_URL" <<CONF
header = "Authorization: Bearer $token"
CONF
}

# Fetch exactly one commit. The auth header lives only in git's environment.
git init -q "$work" || exit 3
if ! GIT_TERMINAL_PROMPT=0 \
    GIT_CONFIG_COUNT=1 \
    GIT_CONFIG_KEY_0=http.extraHeader \
    GIT_CONFIG_VALUE_0="Authorization: Bearer $token" \
    git -C "$work" fetch -q --depth 1 --no-tags "$PIGEON_GIT_URL" "$PIGEON_COMMIT"
then
    exit 3
fi
lap fetch
git -C "$work" checkout -q --detach FETCH_HEAD || exit 3
lap checkout

# The checked-out commit must be the one asked for.
head=$(git -C "$work" rev-parse HEAD) || exit 3
[ "$head" = "$PIGEON_COMMIT" ] || exit 3

# A failed "running" report is not fatal; the final report is.
report '{"state":"running"}' || true

# Finish with a report and map its result to the exit code.
finish() {
    if report "$1"; then
        exit 0
    fi
    exit 4
}

# The script must be a regular file, not a symlink.
if [ ! -f "$work/$PIGEON_SCRIPT" ] || [ -L "$work/$PIGEON_SCRIPT" ]; then
    finish '{"state":"finished","passed":false,"detail":"no such script"}'
fi

# Run the job without the token in its environment, unless the job asks for it (`token = true`
# in .pigeon/ci.toml: the deploy job, whose token the deploy proxy takes only while its landing
# deploys). Its output goes to the container's log and to a file, whose last 4 KiB go with the
# result (the gateway redacts token-shaped text).
# macOS has no `timeout`: there the job runs without it, and the gateway's own limit and
# Buildkite's end the job instead (docs/plans/ios.md, phase 3).
limit=""
if command -v timeout >/dev/null 2>&1; then
    limit="timeout ${PIGEON_TIMEOUT_MIN}m"
fi
cd "$work"
rc=0
if [ "${PIGEON_PASS_TOKEN:-}" = 1 ]; then
    # $limit is unquoted on purpose: empty, or two words of digits and fixed text.
    PIGEON_JOB_TOKEN=$token $limit sh "$work/$PIGEON_SCRIPT" >"$out" 2>&1 || rc=$?
else
    env -u PIGEON_JOB_TOKEN $limit sh "$work/$PIGEON_SCRIPT" >"$out" 2>&1 || rc=$?
fi
cat "$out"
lap script
printf '[timing] total %ss\n' "$(( $(date +%s) - t0 ))" || true
tail=$(tail -c 4096 "$out" | base64 | tr -d '\n')

# The final JSON. Details hold only digits and fixed words; the output is base64.
case $rc in
    0)
        passed=true
        detail=""
        ;;
    124)
        passed=false
        detail="timed out after $PIGEON_TIMEOUT_MIN min"
        ;;
    *)
        passed=false
        detail="exit code $rc"
        ;;
esac
json=$(printf '{"state":"finished","passed":%s,"detail":"%s","exit_code":%s,"output_b64":"%s"}' \
    "$passed" "$detail" "$rc" "$tail")

finish "$json"
