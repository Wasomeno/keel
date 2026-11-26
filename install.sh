#!/bin/sh
# Install the latest Keel release on macOS without browser quarantine prompts.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Wasomeno/keel/main/install.sh | sh
#
# Environment overrides:
#   KEEL_INSTALL_DIR   Destination folder (default: /Applications)
#   KEEL_MANIFEST_URL  Custom latest.json URL (for testing)

set -eu

MANIFEST_URL="${KEEL_MANIFEST_URL:-https://github.com/Wasomeno/keel/releases/latest/download/latest.json}"
INSTALL_DIR="${KEEL_INSTALL_DIR:-/Applications}"
APP_NAME="Keel.app"

fail() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}

[ "$(uname -s)" = "Darwin" ] || fail "Keel installer currently supports macOS only."

RAW_ARCH="$(uname -m)"
case "$RAW_ARCH" in
  arm64)  ARCH_KEY="darwin-aarch64" ;;
  x86_64) ARCH_KEY="darwin-x86_64" ;;
  *)      fail "Unsupported macOS architecture: $RAW_ARCH" ;;
esac

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/keel-install.XXXXXX")"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

json_get() {
  /usr/bin/plutil -extract "$1" raw -o - "$2" 2>/dev/null
}

printf 'Checking latest Keel release...\n'
MANIFEST="$TMP_DIR/latest.json"
curl -fsSL "$MANIFEST_URL" -o "$MANIFEST" || fail "Could not fetch $MANIFEST_URL"

VERSION="$(json_get "version" "$MANIFEST")" || fail "Invalid manifest: missing version"

ASSET_KEY="$ARCH_KEY"
ASSET_URL="$(json_get "assets.${ASSET_KEY}.url" "$MANIFEST" || true)"
if [ -z "$ASSET_URL" ]; then
  ASSET_KEY="darwin-universal"
  ASSET_URL="$(json_get "assets.${ASSET_KEY}.url" "$MANIFEST" || true)"
fi
[ -n "$ASSET_URL" ] || fail "No Keel $VERSION build available for $ARCH_KEY."

ASSET_SHA="$(json_get "assets.${ASSET_KEY}.sha256" "$MANIFEST")" || fail "Invalid manifest: missing sha256"

case "$ASSET_URL" in
  https://*) ;;
  http://127.0.0.1/*|http://localhost/*|http://127.0.0.1:*|http://localhost:*)
    case "$MANIFEST_URL" in
      http://127.0.0.1/*|http://localhost/*|http://127.0.0.1:*|http://localhost:*) ;;
      *) fail "Refusing non-HTTPS asset URL: $ASSET_URL" ;;
    esac
    ;;
  *) fail "Refusing non-HTTPS asset URL: $ASSET_URL" ;;
esac

ARCHIVE="$TMP_DIR/Keel-${VERSION}.tar.xz"
printf 'Downloading Keel %s (%s)...\n' "$VERSION" "$ARCH_KEY"
curl -fL --progress-bar "$ASSET_URL" -o "$ARCHIVE" || fail "Download failed."

printf 'Verifying checksum...\n'
ACTUAL_SHA="$(/usr/bin/shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
if [ "$ACTUAL_SHA" != "$ASSET_SHA" ]; then
  fail "Checksum mismatch (expected $ASSET_SHA, got $ACTUAL_SHA)."
fi

STAGE="$TMP_DIR/stage"
mkdir -p "$STAGE"
printf 'Extracting %s...\n' "$APP_NAME"
/usr/bin/tar -xJf "$ARCHIVE" -C "$STAGE" || fail "Failed to extract archive."
[ -d "$STAGE/$APP_NAME" ] || fail "Archive did not contain $APP_NAME."

/usr/bin/codesign --verify --strict "$STAGE/$APP_NAME" >/dev/null 2>&1 || fail "Code signature verification failed."
/usr/bin/xattr -cr "$STAGE/$APP_NAME" 2>/dev/null || true

TARGET="$INSTALL_DIR/$APP_NAME"
STAGED_TARGET="$INSTALL_DIR/${APP_NAME}.next"
PREV_TARGET="$INSTALL_DIR/${APP_NAME}.previous"

run_install() {
  if [ -w "$INSTALL_DIR" ]; then
    "$@"
  else
    sudo "$@"
  fi
}

mkdir -p "$INSTALL_DIR" 2>/dev/null || sudo mkdir -p "$INSTALL_DIR"
printf 'Installing to %s...\n' "$TARGET"

run_install rm -rf "$STAGED_TARGET" "$PREV_TARGET"
run_install ditto "$STAGE/$APP_NAME" "$STAGED_TARGET"
run_install /usr/bin/xattr -cr "$STAGED_TARGET" 2>/dev/null || true

if [ -e "$TARGET" ]; then
  run_install mv "$TARGET" "$PREV_TARGET"
fi

if ! run_install mv "$STAGED_TARGET" "$TARGET"; then
  if [ -e "$PREV_TARGET" ]; then
    run_install mv "$PREV_TARGET" "$TARGET" || true
  fi
  fail "Could not move $APP_NAME into $INSTALL_DIR."
fi

run_install rm -rf "$PREV_TARGET"

printf '\nKeel %s installed to %s.\n' "$VERSION" "$TARGET"
printf 'Launch it with: open "%s"\n' "$TARGET"
