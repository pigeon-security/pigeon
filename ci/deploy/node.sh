# Sourced by the deploy scripts: puts Node.js 22.12 or later on PATH. Base image v3 has it; an
# older image does not, so the pinned release is fetched once, checked against its published
# SHA-256, and unpacked under $HOME. Keep the version and checksum equal to ci/image/Dockerfile's.
PIGEON_NODE_VERSION=22.23.5
PIGEON_NODE_SHA256=cdeb8670a0c81f51b4dc4ac61919f9b1ad917b304bf8221eb939570663d240f8

pigeon_node_ok() {
    command -v node >/dev/null 2>&1 \
        && node -e 'const [a,b]=process.versions.node.split(".").map(Number);process.exit(a>22||(a===22&&b>=12)?0:1)'
}

if ! pigeon_node_ok; then
    dir=${HOME:-/tmp}/.pigeon-node-$PIGEON_NODE_VERSION
    if [ ! -x "$dir/bin/node" ]; then
        echo "fetching Node.js $PIGEON_NODE_VERSION (this image has no Node 22)" >&2
        tarball=$(mktemp)
        curl -fsSLo "$tarball" \
            "https://nodejs.org/dist/v$PIGEON_NODE_VERSION/node-v$PIGEON_NODE_VERSION-linux-x64.tar.xz"
        echo "$PIGEON_NODE_SHA256  $tarball" | sha256sum -c - >/dev/null
        mkdir -p "$dir"
        tar -xJf "$tarball" -C "$dir" --strip-components=1
        rm -f "$tarball"
    fi
    PATH=$dir/bin:$PATH
    export PATH
    pigeon_node_ok
fi
