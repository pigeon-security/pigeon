#!/bin/sh
# The sleep job: proves a container outlives its runner object going idle. It prints a line each
# minute for five minutes, then passes; a container stopped early ends it as a lost runner.
set -eu
for i in 1 2 3 4 5; do
    sleep 60
    echo "sleep: $i minute(s)"
done
echo "sleep: done"
