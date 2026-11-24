#!/bin/sh
# The warm dependency cache of the CI image (ci/image/Dockerfile v4), for `. ci/runner/warm.sh`
# in test.sh and allowlist.sh. It defines `warm_target [root]` and runs nothing itself.
#
# The image builds every dependency of the workspace into /opt/pigeon-target and records a key in
# /opt/pigeon-target/WARM_KEY: the sha256 of Cargo.lock, a space, and the sha256 of `rustc -Vv`.
# `warm_target` computes the same key for the checkout (root, default the current directory). When
# the two are equal it copies the image's directory to a work directory (the image's may be
# read-only or shared) and exports CARGO_TARGET_DIR there. Otherwise, or when anything fails, it
# changes nothing and the build is cold, as without the image. It never fails the script.
# PIGEON_WARM_DIR names another image directory; PIGEON_WARM_WORK another place for the copy.

# Prints the key for the checkout, or nothing when it cannot be computed.
warm_key() {
    lock=$(sha256sum "${1:-.}/Cargo.lock" 2>/dev/null | cut -d' ' -f1) || return 0
    tool=$(rustc -Vv 2>/dev/null | sha256sum | cut -d' ' -f1) || return 0
    [ -n "$lock" ] && [ -n "$tool" ] || return 0
    printf '%s %s\n' "$lock" "$tool"
}

warm_target() {
    warm_start=$(date +%s)
    warm_dir=${PIGEON_WARM_DIR:-/opt/pigeon-target}
    warm_reason=
    warm_have=$(cat "$warm_dir/WARM_KEY" 2>/dev/null || true)
    warm_now=$(warm_key "${1:-.}" || true)
    if [ ! -f "$warm_dir/WARM_KEY" ]; then
        warm_reason="no image cache at $warm_dir"
    elif [ -z "$warm_now" ]; then
        warm_reason="the key could not be computed"
    elif [ "$warm_have" != "$warm_now" ]; then
        warm_reason="Cargo.lock or the Rust toolchain differs from the image's"
    else
        warm_copy=$(mktemp -d "${PIGEON_WARM_WORK:-${TMPDIR:-/tmp}}/pigeon-warm.XXXXXX" 2>/dev/null || true)
        if [ -n "$warm_copy" ] && cp -a "$warm_dir/." "$warm_copy/" 2>/dev/null; then
            CARGO_TARGET_DIR=$warm_copy
            export CARGO_TARGET_DIR
            echo "[warm] using the image's dependency cache"
        else
            [ -z "$warm_copy" ] || rm -rf "$warm_copy" 2>/dev/null || true
            warm_reason="the cache could not be copied"
        fi
    fi
    [ -z "$warm_reason" ] || echo "[warm] cold build: $warm_reason"
    printf '[timing] warm %ss\n' "$(( $(date +%s) - warm_start ))" || true
    return 0
}
