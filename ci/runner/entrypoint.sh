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

# Keep the token in a plain shell variable (not exported).
token=$PIGEON_JOB_TOKEN
unset PIGEON_JOB_TOKEN v

body=$(mktemp)
trap 'rm -f "$body"' EXIT

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
git init -q /work || exit 3
if ! GIT_TERMINAL_PROMPT=0 \
    GIT_CONFIG_COUNT=1 \
    GIT_CONFIG_KEY_0=http.extraHeader \
    GIT_CONFIG_VALUE_0="Authorization: Bearer $token" \
    git -C /work fetch -q --depth 1 --no-tags "$PIGEON_GIT_URL" "$PIGEON_COMMIT"
then
    exit 3
fi
git -C /work checkout -q --detach FETCH_HEAD || exit 3

# The checked-out commit must be the one asked for.
head=$(git -C /work rev-parse HEAD) || exit 3
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
if [ ! -f "/work/$PIGEON_SCRIPT" ] || [ -L "/work/$PIGEON_SCRIPT" ]; then
    finish '{"state":"finished","passed":false,"detail":"no such script"}'
fi

# Run the job without the token in its environment.
cd /work
rc=0
env -u PIGEON_JOB_TOKEN timeout "${PIGEON_TIMEOUT_MIN}m" sh "/work/$PIGEON_SCRIPT" || rc=$?

# The final JSON. Details hold only digits and fixed words.
case $rc in
    0)
        json='{"state":"finished","passed":true}'
        ;;
    124)
        json=$(printf '{"state":"finished","passed":false,"detail":"timed out after %s min"}' \
            "$PIGEON_TIMEOUT_MIN")
        ;;
    *)
        json=$(printf '{"state":"finished","passed":false,"detail":"exit code %s"}' "$rc")
        ;;
esac

finish "$json"
