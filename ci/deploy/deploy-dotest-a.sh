#!/bin/sh
# The `deploy-dotest-a` job: deploys the pigeon-dotest-a Worker (src/dotest-a), the Durable Object
# transfer rehearsal's source (docs/plans/do-rehearsal.md), as `deploy` deploys core.
set -eu
PIGEON_WORKER_DIR=src/dotest-a exec "$(dirname "$0")/deploy.sh"
