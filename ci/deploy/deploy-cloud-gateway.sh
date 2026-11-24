#!/bin/sh
# The `deploy-cloud-gateway` job: deploys the pigeon-cloud-gateway Worker (src/cloud-gateway) as `deploy` deploys core,
# through the deploy proxy with this job's own token (docs/plans/workers-split.md).
set -eu
PIGEON_WORKER_DIR=src/cloud-gateway exec "$(dirname "$0")/deploy.sh"
