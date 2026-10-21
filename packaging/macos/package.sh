#!/usr/bin/env bash
# Packages the macOS release: a .tar.gz per architecture and a universal
# logship.app in a .dmg, written to dist/.
# Usage: packaging/macos/package.sh <version>
# Expects target/{aarch64,x86_64}-apple-darwin/release/logship.
#
# With a Developer ID certificate and an App Store Connect API key in the
# environment, everything is signed with the hardened runtime, notarized by
# Apple and stapled, so it opens without a Gatekeeper warning:
#
#   MACOS_CERTIFICATE           base64 of the "Developer ID Application" .p12
#   MACOS_CERTIFICATE_PASSWORD  the .p12's password
#   APPLE_API_KEY               contents of the App Store Connect API key (.p8)
#   APPLE_API_KEY_ID            the key's ID
#   APPLE_API_ISSUER_ID         the issuer ID shown above the list of keys
#
# Without them everything is signed ad hoc, which Apple Silicon needs to run
# the code at all, and macOS asks people to allow the app once.
set -euo pipefail

version=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
keychain=$work/signing.keychain-db
targets=(aarch64-apple-darwin x86_64-apple-darwin)
cd "$root"

cleanup() {
  if [ -e "$keychain" ]; then
    security delete-keychain "$keychain" || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

signing=
if [ -n "${MACOS_CERTIFICATE:-}" ]; then
  for var in MACOS_CERTIFICATE_PASSWORD APPLE_API_KEY APPLE_API_KEY_ID APPLE_API_ISSUER_ID; do
    if [ -z "${!var:-}" ]; then
      echo "::error::MACOS_CERTIFICATE is set but $var is not; signing needs all five secrets" >&2
      exit 1
    fi
  done
  signing=1
fi

if [ -n "$signing" ]; then
  # A throwaway keychain holding only the certificate, deleted on exit.
  keychain_password=$(openssl rand -hex 24)
  security create-keychain -p "$keychain_password" "$keychain"
  security set-keychain-settings -lut 3600 "$keychain"
  security unlock-keychain -p "$keychain_password" "$keychain"
  printf '%s' "$MACOS_CERTIFICATE" | base64 --decode >"$work/certificate.p12"
  security import "$work/certificate.p12" -k "$keychain" \
    -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
  rm "$work/certificate.p12"
  security set-key-partition-list -S apple-tool:,apple: -s \
    -k "$keychain_password" "$keychain" >/dev/null
  # codesign looks up identities in the search list.
  # shellcheck disable=SC2046
  security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
  identity=$(security find-identity -v -p codesigning "$keychain" |
    sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | head -n 1)
  if [ -z "$identity" ]; then
    echo '::error::MACOS_CERTIFICATE holds no "Developer ID Application" identity' >&2
    exit 1
  fi
  echo "Signing as $identity"
  printf '%s' "$APPLE_API_KEY" >"$work/api-key.p8"
fi

sign() {
  if [ -n "$signing" ]; then
    codesign --force --options runtime --timestamp --keychain "$keychain" \
      --sign "$identity" "$@"
  else
    codesign --force --sign - "$@"
  fi
}

# Sends a file to Apple's notary service and waits for the verdict.
notarize() {
  local auth=(--key "$work/api-key.p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID")
  local result status
  result=$(xcrun notarytool submit "$1" "${auth[@]}" --wait --timeout 30m --output-format json) || true
  echo "$result"
  status=$(python3 -c 'import json, sys; print(json.load(sys.stdin).get("status", ""))' <<<"$result" || true)
  if [ "$status" != Accepted ]; then
    local id
    id=$(python3 -c 'import json, sys; print(json.load(sys.stdin).get("id", ""))' <<<"$result" || true)
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" "${auth[@]}" || true
    fi
    echo "::error::Apple did not notarize $(basename "$1") (status: ${status:-none})" >&2
    exit 1
  fi
}

out=$work/out
mkdir -p "$out" dist

# A stripped, signed binary per architecture for the tarballs.
for target in "${targets[@]}"; do
  name=logship-$version-$target
  mkdir "$out/$name"
  cp "target/$target/release/logship" LICENSE README.md assets/fonts/LICENSE-NordwandMono.txt \
    "$out/$name/"
  strip "$out/$name/logship"
  sign "$out/$name/logship"
done

# The universal app, with its icon.
app=$out/logship.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/logship" \
  "target/${targets[0]}/release/logship" "target/${targets[1]}/release/logship"
strip "$app/Contents/MacOS/logship"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist >"$app/Contents/Info.plist"
cp packaging/macos/logship.icns LICENSE assets/fonts/LICENSE-NordwandMono.txt \
  "$app/Contents/Resources/"
sign "$app"
codesign --verify --strict --verbose=2 "$app"

if [ -n "$signing" ]; then
  # One submission covers the app and both binaries. The app carries its
  # ticket (stapled); for a bare binary macOS asks Apple online.
  ditto -c -k --keepParent "$out" "$work/notarize.zip"
  notarize "$work/notarize.zip"
  xcrun stapler staple "$app"
  spctl --assess --type execute --verbose "$app"
fi

for target in "${targets[@]}"; do
  name=logship-$version-$target
  tar -czf "dist/$name.tar.gz" -C "$out" "$name"
done

dmg=dist/logship-$version-macos-universal.dmg
mkdir "$work/dmg"
ditto "$app" "$work/dmg/logship.app"
ln -s /Applications "$work/dmg/Applications"
hdiutil create -volname logship -srcfolder "$work/dmg" -ov -format UDZO "$dmg"
if [ -n "$signing" ]; then
  codesign --force --timestamp --keychain "$keychain" --sign "$identity" "$dmg"
  notarize "$dmg"
  xcrun stapler staple "$dmg"
  spctl --assess --type open --context context:primary-signature --verbose "$dmg"
fi
