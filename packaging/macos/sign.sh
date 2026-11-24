#!/usr/bin/env bash
set -euo pipefail
APP="$1"
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -z "${MACOS_CERT_P12:-}" ] || [ -z "${MACOS_CERT_PASSWORD:-}" ]; then
  echo "::error::MACOS_CERT_P12 and MACOS_CERT_PASSWORD secrets are missing; an ad-hoc signature would make macOS ask for the keychain password after every update."
  exit 1
fi
HASH=$(openssl x509 -in "$HERE/signing-cert.pem" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')
KC="$RUNNER_TEMP/signing.keychain-db"
KCPW=$(openssl rand -hex 16)
echo "$MACOS_CERT_P12" | base64 --decode > "$RUNNER_TEMP/sign.p12"
security create-keychain -p "$KCPW" "$KC"
security set-keychain-settings -lut 3600 "$KC"
security unlock-keychain -p "$KCPW" "$KC"
security import "$RUNNER_TEMP/sign.p12" -k "$KC" -P "$MACOS_CERT_PASSWORD" -T /usr/bin/codesign
rm -f "$RUNNER_TEMP/sign.p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KCPW" "$KC" > /dev/null
security list-keychains -d user -s "$KC" $(security list-keychains -d user | tr -d '"')
if ! codesign --force --deep --keychain "$KC" --sign "$HASH" "$APP"; then
  sudo security add-trusted-cert -d -r trustRoot -p codeSign -k "$KC" "$HERE/signing-cert.pem"
  codesign --force --deep --keychain "$KC" --sign "$HASH" "$APP"
fi
codesign --verify --deep --strict "$APP"
REQ=$(codesign -d -r- "$APP" 2>&1)
echo "$REQ"
LOWER=$(echo "$HASH" | tr 'A-F' 'a-f')
if echo "$REQ" | grep -q cdhash || ! echo "$REQ" | grep -q 'identifier "io.github.schrumbo.manycraft"' || ! echo "$REQ" | grep -qi "H\"$LOWER\""; then
  echo "::error::the app is not signed with the Manycraft certificate; macOS would ask for keychain access after every update."
  exit 1
fi
security delete-keychain "$KC"
