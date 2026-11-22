#!/bin/sh
# The `deploy-sites` job: deploys the pigeon-sites Worker (src/sites) as `deploy` deploys core,
# through the deploy proxy with this job's own token (docs/plans/workers-split.md).
set -eu
PIGEON_WORKER_DIR=src/sites exec "$(dirname "$0")/deploy.sh"
