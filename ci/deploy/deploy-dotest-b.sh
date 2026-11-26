#!/bin/sh
# The `deploy-dotest-b` job: deploys the pigeon-dotest-b Worker (src/dotest-b), the Durable Object
# transfer rehearsal's destination (docs/plans/do-rehearsal.md), as `deploy` deploys core.
set -eu
PIGEON_WORKER_DIR=src/dotest-b exec "$(dirname "$0")/deploy.sh"
