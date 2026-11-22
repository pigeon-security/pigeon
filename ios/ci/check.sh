#!/usr/bin/env bash
# Builds the iOS app for the simulator, unsigned. Runs on a macOS agent only, with the Xcode of
# ios/.xcode-version (docs/plans/ios.md). The Rust gate never runs it.
#
# The hosted image has Xcode but not XcodeGen, so a pinned release is fetched and checked by its
# SHA-256 before it runs.
set -euo pipefail
cd "$(dirname "$0")/.."

XCODEGEN_VERSION=2.44.1
XCODEGEN_SHA256=a2e905fb68446e9bb4008cdfe2e13e3f176d0cbcca828b71770f8e53fca91b73

if command -v xcodegen >/dev/null 2>&1; then
  xcodegen=xcodegen
else
  tools=$(mktemp -d)
  trap 'rm -rf "$tools"' EXIT
  curl -fsSL --retry 3 --max-time 120 -o "$tools/xcodegen.zip" \
    "https://github.com/yonaskolb/XcodeGen/releases/download/$XCODEGEN_VERSION/xcodegen.zip"
  echo "$XCODEGEN_SHA256  $tools/xcodegen.zip" | shasum -a 256 -c -
  unzip -q "$tools/xcodegen.zip" -d "$tools"
  xcodegen="$tools/xcodegen/bin/xcodegen"
fi

echo "Xcode: $(xcodebuild -version | head -1); XcodeGen: $("$xcodegen" --version)"
"$xcodegen" generate
xcodebuild build \
  -project Pigeon.xcodeproj \
  -scheme Pigeon \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath build \
  CODE_SIGNING_ALLOWED=NO
