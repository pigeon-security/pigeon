#!/bin/sh
# The smoke job: the commit is checked out and a script runs. Nothing else.
set -eu
test -f .pigeon/ci.toml
echo "smoke: $(git rev-parse HEAD 2>/dev/null || echo 'checked out')"
