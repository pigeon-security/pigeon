#!/usr/bin/env bash
# One iOS release (docs/plans/ios.md, phase 4): CI job `ios-release`, at main's head, on
# Buildkite's hosted macOS (a fresh machine per job). Its job token is its only credential.
#
# 1. Asks core for the release's build number (/apple/release/start).
# 2. Archives the app unsigned, so no build phase runs while a key is on the machine.
# 3. Asks core for pigeon's signing identity, once (/apple/release/identity), into a temporary
#    keychain with a random password; exports the signed IPA.
# 4. Sends the IPA to core (/apple/release/upload), which uploads it to App Store Connect. The
#    Mac never holds the App Store Connect key or a token for it.
#
# The EXIT trap deletes the keychain, the key, the profile and the work files.
set -euo pipefail
cd "$(dirname "$0")/.."

: "${PIGEON_JOB_TOKEN:?the release needs its job token (token = true in .pigeon/ci.toml)}"
base="${PIGEON_RUNNER_URL%%/ci/runner/*}"
case $base in https://*) ;; *) echo "pigeon: no platform URL" >&2; exit 2 ;; esac

. ci/xcodegen.sh
secret=$(mktemp -d)
keychain="$secret/release.keychain-db"
profiles=("$HOME/Library/MobileDevice/Provisioning Profiles" "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles")
cleanup() {
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  if [ -n "${uuid:-}" ]; then
    for d in "${profiles[@]}"; do rm -f "$d/$uuid.mobileprovision"; done
  fi
  rm -rf "$secret" "$tools" build/export
}
trap cleanup EXIT

# The token goes to curl on stdin, never on its command line.
call() { # call STEP [curl args...]
  local step=$1; shift
  curl --config - -fsS --retry 2 --max-time 600 -X POST "$@" "$base/apple/release/$step" <<CONF
header = "Authorization: Bearer $PIGEON_JOB_TOKEN"
CONF
}
field() { /usr/bin/plutil -extract "$1" raw -o - -- "$2"; }

call start -o "$secret/start.json"
number=$(field build_number "$secret/start.json")
echo "pigeon: release build $number"

echo "Xcode: $(xcodebuild -version | head -1); XcodeGen: $("$xcodegen" --version)"
"$xcodegen" generate
xcodebuild archive \
  -project Pigeon.xcodeproj \
  -scheme Pigeon \
  -configuration Release \
  -destination 'generic/platform=iOS' \
  -archivePath build/Pigeon.xcarchive \
  -derivedDataPath build/derived \
  CODE_SIGNING_ALLOWED=NO \
  CURRENT_PROJECT_VERSION="$number"
version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
  build/Pigeon.xcarchive/Products/Applications/Pigeon.app/Info.plist)
# The unsigned archive carries no entitlements, and the export keeps only those already in the
# app's signature. An ad-hoc signature (no key) with the app's entitlements carries them over.
codesign --force --sign - --entitlements Pigeon/Pigeon.entitlements \
  build/Pigeon.xcarchive/Products/Applications/Pigeon.app

# The identity, once. Nothing below prints it.
call identity -o "$secret/identity.json"
team=$(field team_id "$secret/identity.json")
uuid=$(field profile_uuid "$secret/identity.json")
field private_key "$secret/identity.json" > "$secret/key.pem"
field certificate "$secret/identity.json" | base64 -D > "$secret/cert.der"
for d in "${profiles[@]}"; do
  mkdir -p "$d"
  field profile "$secret/identity.json" | base64 -D > "$d/$uuid.mobileprovision"
done
rm -f "$secret/identity.json"

# The system's LibreSSL, not a newer OpenSSL on PATH: OpenSSL 3 writes a .p12 (AES, SHA-256
# MAC) that `security import` cannot read; LibreSSL writes the 3DES and SHA-1 form it can.
ssl=/usr/bin/openssl
pw=$($ssl rand -hex 24)
$ssl x509 -inform DER -in "$secret/cert.der" -out "$secret/cert.pem"
$ssl pkcs12 -export -inkey "$secret/key.pem" -in "$secret/cert.pem" \
  -out "$secret/id.p12" -passout "pass:$pw"
rm -f "$secret/key.pem"
security create-keychain -p "$pw" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$pw" "$keychain"
security import "$secret/id.p12" -k "$keychain" -P "$pw" -T /usr/bin/codesign -T /usr/bin/security
rm -f "$secret/id.p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$pw" "$keychain" >/dev/null
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')

# Apple's WWDR G3 intermediate: used from the image when present, otherwise fetched and
# accepted only if it verifies against the system's Apple root.
if ! security find-certificate -c "Apple Worldwide Developer Relations Certification Authority" \
    /Library/Keychains/System.keychain "$HOME/Library/Keychains/login.keychain-db" >/dev/null 2>&1; then
  curl -fsSL --retry 3 --max-time 60 -o "$secret/wwdr.cer" \
    https://www.apple.com/certificateauthority/AppleWWDRCAG3.cer
  security verify-cert -c "$secret/wwdr.cer" -p basic
  security import "$secret/wwdr.cer" -k "$keychain"
fi
security find-identity -v -p codesigning "$keychain"

cat > "$secret/export.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>method</key><string>app-store-connect</string>
  <key>destination</key><string>export</string>
  <key>teamID</key><string>$team</string>
  <key>signingStyle</key><string>manual</string>
  <key>signingCertificate</key><string>Apple Distribution</string>
  <key>provisioningProfiles</key><dict><key>dev.pigeon.app</key><string>$uuid</string></dict>
  <key>manageAppVersionAndBuildNumber</key><false/>
  <key>uploadSymbols</key><false/>
</dict></plist>
PLIST
xcodebuild -exportArchive \
  -archivePath build/Pigeon.xcarchive \
  -exportPath build/export \
  -exportOptionsPlist "$secret/export.plist" \
  OTHER_CODE_SIGN_FLAGS="--keychain $keychain"
ls -l build/export/Pigeon.ipa
# What was signed, for the log: the signer and the entitlements (none of it secret).
(cd "$secret" && unzip -q "$OLDPWD/build/export/Pigeon.ipa" 'Payload/*' \
  && codesign -dv --verbose=2 Payload/Pigeon.app 2>&1 | grep -E "^(Authority|TeamIdentifier)" \
  && codesign -d --entitlements - --xml Payload/Pigeon.app 2>/dev/null | plutil -p - ; rm -rf Payload) || true

call "upload?version=$version" --data-binary @build/export/Pigeon.ipa \
  -H 'Content-Type: application/octet-stream' -o "$secret/upload.json"
cat "$secret/upload.json"; echo
echo "pigeon: build $number ($version) is uploaded; ios_releases shows Apple's processing"
