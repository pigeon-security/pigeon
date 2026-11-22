#!/bin/sh
# The `deploy-model-gateway` job: deploys the pigeon-model-gateway Worker (src/model-gateway) as `deploy` deploys core,
# through the deploy proxy with this job's own token (docs/plans/workers-split.md).
set -eu
PIGEON_WORKER_DIR=src/model-gateway exec "$(dirname "$0")/deploy.sh"
