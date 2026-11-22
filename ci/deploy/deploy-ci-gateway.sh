#!/bin/sh
# The `deploy-ci-gateway` job: deploys the pigeon-ci-gateway Worker (src/ci-gateway) as `deploy` deploys core,
# through the deploy proxy with this job's own token (docs/plans/workers-split.md).
set -eu
PIGEON_WORKER_DIR=src/ci-gateway exec "$(dirname "$0")/deploy.sh"
