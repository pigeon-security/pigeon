#!/bin/sh
# The `deploy-proxy` job: deploys the pigeon-proxy Worker (src/proxy) as `deploy` deploys core,
# through the deploy proxy with this job's own token (docs/plans/workers-split.md).
set -eu
PIGEON_WORKER_DIR=src/proxy exec "$(dirname "$0")/deploy.sh"
