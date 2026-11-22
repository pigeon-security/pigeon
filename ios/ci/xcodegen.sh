# Sourced by check.sh and release.sh: sets $xcodegen to an XcodeGen binary. The hosted image has
# Xcode but not XcodeGen, so a pinned release is fetched and checked by its SHA-256 before it
# runs. Leaves $tools (a temporary directory) for the caller's EXIT trap to remove.
XCODEGEN_VERSION=2.44.1
XCODEGEN_SHA256=a2e905fb68446e9bb4008cdfe2e13e3f176d0cbcca828b71770f8e53fca91b73

tools=$(mktemp -d)
if command -v xcodegen >/dev/null 2>&1; then
  xcodegen=xcodegen
else
  curl -fsSL --retry 3 --max-time 120 -o "$tools/xcodegen.zip" \
    "https://github.com/yonaskolb/XcodeGen/releases/download/$XCODEGEN_VERSION/xcodegen.zip"
  echo "$XCODEGEN_SHA256  $tools/xcodegen.zip" | shasum -a 256 -c -
  unzip -q "$tools/xcodegen.zip" -d "$tools"
  xcodegen="$tools/xcodegen/bin/xcodegen"
fi
