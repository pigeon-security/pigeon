#!/bin/sh
# The failing smoke job: prints a line, then exits 1, so the failed path, the exit code, the
# output tail and notify signing can be checked live.
echo "smoke-fail: failing on purpose at $(git rev-parse HEAD 2>/dev/null || echo 'this commit')"
exit 1
