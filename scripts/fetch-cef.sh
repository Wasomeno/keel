#!/bin/bash
# Fetch pinned CEF binary distribution for the current platform into .cef
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CEF_DIR="$ROOT_DIR/.cef"

OS="$(uname -s)"
ARCH="$(uname -m)"

CEF_VER="156.0.14+g4fa4ab5+chromium-156.0.8078.48"

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64|aarch64)
        PLATFORM="macosarm64"
        DIR_NAME="cef_macos_aarch64"
        ;;
      x86_64)
        PLATFORM="macosx64"
        DIR_NAME="cef_macos_x86_64"
        ;;
      *)
        echo "Unsupported macOS architecture: $ARCH" >&2
        exit 1
        ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64)
        PLATFORM="linux64"
        DIR_NAME="cef_linux_x86_64"
        ;;
      aarch64)
        PLATFORM="linuxarm64"
        DIR_NAME="cef_linux_aarch64"
        ;;
      *)
        echo "Unsupported Linux architecture: $ARCH" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS" >&2
    exit 1
    ;;
esac

DEST="$CEF_DIR/$DIR_NAME"
VERSIONED_DEST="$CEF_DIR/156.0.14/$DIR_NAME"

if [ -f "$DEST/archive.json" ] || [ -f "$VERSIONED_DEST/archive.json" ]; then
  echo "CEF binary already downloaded in $CEF_DIR"
  exit 0
fi

ARCHIVE_NAME="cef_binary_${CEF_VER}_${PLATFORM}_minimal.tar.bz2"
URL="https://cef-builds.spotifycdn.com/$ARCHIVE_NAME"

mkdir -p "$CEF_DIR"
TMP_ARCHIVE="$CEF_DIR/$ARCHIVE_NAME"

echo "Downloading CEF from $URL..."
curl -L --progress-bar -o "$TMP_ARCHIVE" "$URL"

echo "Extracting $ARCHIVE_NAME..."
TMP_EXTRACT="$CEF_DIR/tmp_extract"
rm -rf "$TMP_EXTRACT"
mkdir -p "$TMP_EXTRACT"
tar -xjf "$TMP_ARCHIVE" -C "$TMP_EXTRACT" --strip-components=1

rm -rf "$DEST"
mkdir -p "$DEST"

if [ "$OS" = "Darwin" ]; then
  cp -R "$TMP_EXTRACT/Release/"* "$DEST/"
else
  cp -R "$TMP_EXTRACT/Release/"* "$DEST/"
  if [ -d "$TMP_EXTRACT/Resources" ]; then
    cp -R "$TMP_EXTRACT/Resources/"* "$DEST/"
  fi
fi

cp "$TMP_EXTRACT/CMakeLists.txt" "$DEST/"
cp -R "$TMP_EXTRACT/cmake" "$DEST/"
cp -R "$TMP_EXTRACT/include" "$DEST/"
cp -R "$TMP_EXTRACT/libcef_dll" "$DEST/"
if [ -f "$TMP_EXTRACT/CREDITS.html" ]; then
  cp "$TMP_EXTRACT/CREDITS.html" "$DEST/"
fi

cat > "$DEST/archive.json" <<EOF
{
  "type": "minimal",
  "name": "$ARCHIVE_NAME",
  "sha1": ""
}
EOF

mkdir -p "$CEF_DIR/156.0.14"
ln -sfn "../$DIR_NAME" "$CEF_DIR/156.0.14/$DIR_NAME"

rm -rf "$TMP_EXTRACT" "$TMP_ARCHIVE"
echo "CEF successfully fetched to $DEST"
