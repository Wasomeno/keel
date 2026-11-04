#!/bin/bash
# Wrap the release binary in a minimal Keel.app (bundle id com.keel.dev) and
# ad-hoc sign it. macOS only shows clickable notifications, under Keel's name,
# for a bundled app; the LaunchAgent runs Contents/MacOS/keel from here.
#
#   cargo build --release -p keel && scripts/bundle-mac.sh
#   launchctl kickstart -k gui/$(id -u)/com.keel.dev
#
# KEEL_APP overrides the destination (default ~/Applications/Keel.app),
# KEEL_BIN the binary (default target/release/keel).
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${KEEL_BIN:-target/release/keel}
APP=${KEEL_APP:-$HOME/Applications/Keel.app}
[ -x "$BIN" ] || { echo "no $BIN: cargo build --release -p keel first" >&2; exit 1; }
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
# A real copy, not a symlink: the bundle must own its executable for the
# signature (and the running app must not change under a rebuild). Copy to
# a temp name and rename, so a running Keel keeps its old file.
cp "$BIN" "$APP/Contents/MacOS/.keel.new"
mv -f "$APP/Contents/MacOS/.keel.new" "$APP/Contents/MacOS/keel"

# The pinned Pi (and its bundled packages) that Keel's agent runs on.
scripts/fetch-pi.sh "$APP/Contents/Resources"
# Clean non-runtime assets from bundled packages (demo videos, documentation, banners, source maps)
find "$APP/Contents/Resources/pi-packages" -type f \( -name "*.mp4" -o -name "*.png" -o -name "*.map" \) -delete 2>/dev/null || true
rm -rf "$APP/Contents/Resources/pi/examples" "$APP/Contents/Resources/pi/docs" 2>/dev/null || true

# CEF framework and helpers
CEF_SRC="$(pwd)/.cef/cef_macos_aarch64/Chromium Embedded Framework.framework"
if [ -d "$CEF_SRC" ]; then
  mkdir -p "$APP/Contents/Frameworks"
  if [ ! -d "$APP/Contents/Frameworks/Chromium Embedded Framework.framework" ]; then
    cp -R "$CEF_SRC" "$APP/Contents/Frameworks/"
  fi
fi

HELPER_BIN="$(dirname "$BIN")/keel-helper"
HELPERS=(
  "Keel Helper:com.keel.dev.helper"
  "Keel Helper (GPU):com.keel.dev.helper.gpu"
  "Keel Helper (Renderer):com.keel.dev.helper.renderer"
  "Keel Helper (Plugin):com.keel.dev.helper.plugin"
  "Keel Helper (Alerts):com.keel.dev.helper.alerts"
)

for ENTRY in "${HELPERS[@]}"; do
  HNAME="${ENTRY%%:*}"
  HID="${ENTRY##*:}"
  HDIR="$APP/Contents/Frameworks/$HNAME.app"
  mkdir -p "$HDIR/Contents/MacOS"
  if [ -f "$HELPER_BIN" ]; then
    cp "$HELPER_BIN" "$HDIR/Contents/MacOS/.keel-helper.new"
    mv -f "$HDIR/Contents/MacOS/.keel-helper.new" "$HDIR/Contents/MacOS/keel-helper"
  fi
  cat > "$HDIR/Contents/Info.plist" <<HPLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>$HID</string>
  <key>CFBundleName</key><string>$HNAME</string>
  <key>CFBundleDisplayName</key><string>$HNAME</string>
  <key>CFBundleExecutable</key><string>keel-helper</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSUIElement</key><true/>
</dict></plist>
HPLIST
done

ICON_KEYS=""
if [ -f assets/Keel.icns ]; then
  cp assets/Keel.icns "$APP/Contents/Resources/Keel.icns"
  ICON_KEYS="<key>CFBundleIconFile</key><string>Keel</string>"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.keel.dev</string>
  <key>CFBundleName</key><string>Keel</string>
  <key>CFBundleDisplayName</key><string>Keel</string>
  <key>CFBundleExecutable</key><string>keel</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><false/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  $ICON_KEYS
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>com.keel.dev.link</string>
      <key>CFBundleURLSchemes</key><array><string>keel</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Inside out, never --deep: --deep re-signs nested bundles wrong and is the
# usual reason --verify --strict fails after a copy (UPDATER.md). Order: the
# CEF framework's dylibs, the framework, each helper .app, the outer app.
sign_bundle() {
  local identity=$1 app=$2 fw="$2/Contents/Frameworks"
  local cef="$fw/Chromium Embedded Framework.framework"
  if [ -d "$cef" ]; then
    if [ -d "$cef/Libraries" ]; then
      find "$cef/Libraries" -type f -name '*.dylib' -print0 |
        while IFS= read -r -d '' lib; do codesign --force --sign "$identity" "$lib"; done
    fi
    codesign --force --sign "$identity" "$cef"
  fi
  for helper in "$fw"/*.app; do
    [ -d "$helper" ] && codesign --force --sign "$identity" "$helper"
  done
  codesign --force --sign "$identity" "$app"
}

# A stable identity keeps the Keychain's "Always Allow" across rebuilds; an
# ad-hoc signature is new every build, so macOS asks for the password again.
# KEEL_REQUIRE_IDENTITY=1 (publish-mac.sh) refuses ad hoc: an update archive
# must carry a certificate signature.
IDENTITY="${KEEL_SIGN_IDENTITY:-Keel Dev}"
if [ "$IDENTITY" != "-" ] && security find-identity -p codesigning | grep -q "\"$IDENTITY\""; then
  sign_bundle "$IDENTITY" "$APP"
elif [ -n "${KEEL_REQUIRE_IDENTITY:-}" ]; then
  echo "no \"$IDENTITY\" signing certificate; refusing to sign an update build ad hoc" >&2
  exit 1
else
  echo "no \"$IDENTITY\" signing certificate; signing ad hoc (Keychain will ask after each rebuild)"
  sign_bundle - "$APP"
fi
codesign --verify --strict "$APP"
# A build copy (KEEL_APP set) is not registered as the Keel that opens keel:// links.
[ -n "${KEEL_APP:-}" ] || /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP"
echo "bundled $APP ($VERSION)"
