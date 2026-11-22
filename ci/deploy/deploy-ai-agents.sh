#!/bin/sh
# The `deploy-ai-agents` job: deploys the pigeon-ai-agents Worker (src/ai-agents-worker) as `deploy` deploys core,
# through the deploy proxy with this job's own token (docs/plans/ai-agents.md).
set -eu
PIGEON_WORKER_DIR=src/ai-agents-worker exec "$(dirname "$0")/deploy.sh"
