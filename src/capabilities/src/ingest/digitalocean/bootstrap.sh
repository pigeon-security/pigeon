#!/bin/bash
# Pigeon renders validated assignments immediately before this script.
set -euo pipefail
umask 077
trap 'rm -f /run/pigeon-ingest-creds /run/pigeon-ingest.env; poweroff -f' EXIT

device="/dev/disk/by-id/scsi-0DO_Volume_${VOLUME_NAME}"
mkdir -p /spool
for attempt in $(seq 1 120); do
    [ ! -b "$device" ] || break
    sleep 1
done
test -b "$device"
# DigitalOcean can auto-mount a preformatted volume on the approved device.
mounted="$(findmnt --noheadings --raw --source "$(readlink -f "$device")" --output TARGET || true)"
if [ -n "$mounted" ]; then
    mount --bind "${mounted%%$'\n'*}" /spool
    mount -o remount,bind,nodev,nosuid /spool
else
    mount -t ext4 -o nodev,nosuid "$device" /spool
fi
mountpoint -q /spool
chown 10001:10001 /spool
chmod 700 /spool
test "$(df -PB1 /spool | awk 'NR==2 {print $4}')" -ge "$SPOOL_BYTES"

export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y --no-install-recommends docker.io curl git python3 ca-certificates
systemctl start docker
docker build --build-arg "PIGEON_COMMIT=$COMMIT" -t pigeon-ingest     -f src/ingest-runner/Dockerfile "https://github.com/pigeon-security/pigeon.git#$COMMIT"
curl --fail --silent --show-error --retry 8 --retry-all-errors --max-time 20 \
    --header "Authorization: Bearer $BOOT_TOKEN" \
    --header 'Content-Type: application/json' \
    --data "{\"run\":\"$RUN\"}" "$BOOT_URL" >/run/pigeon-ingest-creds
python3 - /run/pigeon-ingest-creds /run/pigeon-ingest.env <<'PYTHON'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    credentials = json.load(source)
with open(sys.argv[2], "w", encoding="utf-8") as target:
    target.write("INGEST_JOB=" + json.dumps(credentials["job"], separators=(",", ":")) + "\n")
    if "INGEST_DESTINATION" in credentials:
        if "DEST_KEYS" in credentials:
            raise ValueError("conflicting ingest destination")
        target.write("INGEST_DESTINATION=" + json.dumps(credentials["INGEST_DESTINATION"], separators=(",", ":")) + "\n")
        variables = ("SOURCE_KEYS",)
    else:
        variables = ("SOURCE_KEYS", "DEST_KEYS")
    for variable in variables:
        key = credentials[variable]
        if not key or "\r" in key or "\n" in key:
            raise ValueError("invalid ingest credential")
        target.write(variable + "=" + key + "\n")
PYTHON
rm -f /run/pigeon-ingest-creds
remaining=$(( DEADLINE_SECONDS - $(date +%s) ))
test "$remaining" -gt 0
timeout --signal=TERM --kill-after=60 "$remaining" \
    docker run --rm --env-file /run/pigeon-ingest.env --env "SPOOL_BYTES=$SPOOL_BYTES" \
    --mount type=bind,source=/spool,target=/spool pigeon-ingest
