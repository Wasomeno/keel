#!/bin/bash
# Build the two files a person attaches to a GitHub release so installed
# Keels can update themselves (UPDATER.md). Uploads nothing.
#
#   KEEL_SIGN_IDENTITY="Keel Release" \
#   KEEL_UPDATE_SECRET=~/keel-signing/keel-update.key scripts/publish-mac.sh
#
# Attach the archive first, then latest.json: a client that sees a new
# manifest must already be able to download the archive.
#
# One time, on the machine that builds releases:
#
# 1. The code-signing certificate. Keychain Access > Certificate Assistant >
#    Create a Certificate. Name "Keel Release", Identity Type Self Signed
#    Root, Certificate Type Code Signing. Then export it (with its private
#    key) as a .p12 and keep that file and its password outside the repo.
#
# 2. The update key. Writes the public half into the repo and the secret
#    half somewhere else (never commit it):
#
#      minisign -G -p apps/keel/update-key.pub -s ~/keel-signing/keel-update.key
#
#    Commit apps/keel/update-key.pub only. A build compiled with the
#    placeholder in that file refuses every update.
#
# KEEL_UPDATE_SECRET_PASSWORD, when set, is the secret key's password; it is
# read from the environment only. KEEL_NOTARY_PROFILE is ignored here.
set -euo pipefail
cd "$(dirname "$0")/.."

IDENTITY=${KEEL_SIGN_IDENTITY:-}
[ -n "$IDENTITY" ] && [ "$IDENTITY" != "-" ] ||
  { echo "set KEEL_SIGN_IDENTITY to a certificate (not -); an update must not be ad hoc" >&2; exit 1; }
[ -n "${KEEL_UPDATE_SECRET:-}" ] && [ -f "$KEEL_UPDATE_SECRET" ] ||
  { echo "set KEEL_UPDATE_SECRET to the minisign secret key file" >&2; exit 1; }
command -v minisign >/dev/null || { echo "minisign not found (brew install minisign)" >&2; exit 1; }
if grep -q PLACEHOLDER apps/keel/update-key.pub; then
  echo "apps/keel/update-key.pub is still the placeholder; see the comment at the top of this script" >&2
  exit 1
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET=aarch64-apple-darwin
NAME="Keel-$VERSION-macos-arm64.tar.xz"
OUT=target/publish/$VERSION
TAG="v$VERSION"
REPO_URL=https://github.com/Wasomeno/keel

cargo build --release -p keel --target "$TARGET"

STAGE=$(mktemp -d)
CHECK=$(mktemp -d)
trap 'rm -rf "$STAGE" "$CHECK"' EXIT

KEEL_APP="$STAGE/Keel.app" KEEL_BIN="target/$TARGET/release/keel" \
  KEEL_SIGN_IDENTITY="$IDENTITY" KEEL_REQUIRE_IDENTITY=1 scripts/bundle-mac.sh
codesign --verify --strict --verbose=2 "$STAGE/Keel.app"

mkdir -p "$OUT"
ARCHIVE="$OUT/$NAME"
rm -f "$ARCHIVE" "$ARCHIVE.minisig" "$OUT/latest.json"
# --no-xattrs keeps com.apple.quarantine out; symlinks stay links.
tar --no-xattrs -cJf "$ARCHIVE" -C "$STAGE" Keel.app
SHA=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
SIZE=$(stat -f %z "$ARCHIVE")

if [ -n "${KEEL_UPDATE_SECRET_PASSWORD:-}" ]; then
  printf '%s\n' "$KEEL_UPDATE_SECRET_PASSWORD" |
    minisign -S -s "$KEEL_UPDATE_SECRET" -m "$ARCHIVE" -x "$ARCHIVE.minisig" >/dev/null
else
  minisign -S -s "$KEEL_UPDATE_SECRET" -m "$ARCHIVE" -x "$ARCHIVE.minisig"
fi
minisign -V -p apps/keel/update-key.pub -m "$ARCHIVE" -x "$ARCHIVE.minisig" >/dev/null

KEEL_VERSION="$VERSION" KEEL_NOTES="$REPO_URL/releases/tag/$TAG" \
KEEL_URL="$REPO_URL/releases/download/$TAG/$NAME" KEEL_SHA="$SHA" KEEL_SIZE="$SIZE" \
KEEL_SIG="$ARCHIVE.minisig" /usr/bin/python3 - "$OUT/latest.json" <<'PY'
import json, os, sys
env = os.environ
manifest = {
    "version": env["KEEL_VERSION"],
    "notes_url": env["KEEL_NOTES"],
    "min_os": "12.0",
    "assets": {
        "darwin-aarch64": {
            "url": env["KEEL_URL"],
            "sha256": env["KEEL_SHA"],
            "size": int(env["KEEL_SIZE"]),
            "signature": open(env["KEEL_SIG"]).read(),
        }
    },
}
with open(sys.argv[1], "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PY

# The check that symlinks survived: what users extract must still verify.
tar -xJf "$ARCHIVE" -C "$CHECK"
[ "$(ls "$CHECK")" = Keel.app ] || { echo "archive has more than Keel.app at the top" >&2; exit 1; }
codesign --verify --strict --verbose=2 "$CHECK/Keel.app"

DMG="$OUT/Keel-$VERSION-macos-arm64.dmg"
ZIP="$OUT/Keel-$VERSION-macos-arm64.zip"
rm -f "$DMG" "$ZIP"

echo "building $DMG..."
DMG_STAGE=$(mktemp -d)
cp -R "$STAGE/Keel.app" "$DMG_STAGE/"
ln -s /Applications "$DMG_STAGE/Applications"
hdiutil create -volname Keel -srcfolder "$DMG_STAGE" -format UDZO -ov "$DMG" >/dev/null
rm -rf "$DMG_STAGE"

echo "building $ZIP..."
ditto -c -k --keepParent "$STAGE/Keel.app" "$ZIP"

echo "upload, in this order:"
echo "$ARCHIVE"
echo "$DMG"
echo "$ZIP"
echo "$OUT/latest.json"
