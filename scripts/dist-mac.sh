#!/bin/bash
# Build a Keel.dmg anyone can install: one universal binary (Apple Silicon and
# Intel), signed with the hardened runtime, notarized and stapled.
#
#   KEEL_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
#   KEEL_NOTARY_PROFILE=keel-notary scripts/dist-mac.sh
#
# The notary profile is made once with
#   xcrun notarytool store-credentials keel-notary --apple-id you@example.com --team-id TEAMID
# Without a Developer ID the app is signed ad hoc: it runs here, but other Macs
# need right-click > Open the first time. Output: target/dist/Keel-<version>.dmg.
#
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=target/dist
APP=$OUT/Keel.app
DMG=$OUT/Keel-$VERSION.dmg
# Default: the local "Keel Dev" identity if present (stable Keychain access), else ad hoc.
IDENTITY=${KEEL_SIGN_IDENTITY:-$(security find-identity -p codesigning | grep -q '"Keel Dev"' && echo "Keel Dev" || echo -)}

# One target at a time: each is a full release build.
for t in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release -p keel --target "$t"
done
mkdir -p target/universal
lipo -create -output target/universal/keel \
  target/aarch64-apple-darwin/release/keel target/x86_64-apple-darwin/release/keel

rm -rf "$APP"
KEEL_APP="$APP" KEEL_BIN=target/universal/keel KEEL_SIGN_IDENTITY="$IDENTITY" scripts/bundle-mac.sh
# Notarization requires the hardened runtime and a secure timestamp.
TS=--timestamp; [[ "$IDENTITY" = Developer\ ID* ]] || TS=--timestamp=none
codesign --force --options runtime "$TS" --sign "$IDENTITY" "$APP"
codesign --verify --strict --verbose=2 "$APP"

STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname Keel -srcfolder "$STAGE" -format UDZO -ov "$DMG" >/dev/null
rm -rf "$STAGE"
[[ "$IDENTITY" = Developer\ ID* ]] && codesign --force --timestamp --sign "$IDENTITY" "$DMG"

if [ -n "${KEEL_NOTARY_PROFILE:-}" ] && [[ "$IDENTITY" = Developer\ ID* ]]; then
  xcrun notarytool submit "$DMG" --keychain-profile "$KEEL_NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG"
  spctl --assess --type open --context context:primary-signature -v "$DMG"
else
  echo "not notarized: set KEEL_SIGN_IDENTITY (Developer ID) and KEEL_NOTARY_PROFILE"
fi
# The per-arch release builds are gigabytes and only fed the universal
# binary; the DMG is the product. Set KEEL_KEEP_BUILD=1 to keep them for a
# faster next release.
if [ -z "${KEEL_KEEP_BUILD:-}" ]; then
  rm -rf target/aarch64-apple-darwin target/x86_64-apple-darwin target/universal
fi
echo "built $DMG"
