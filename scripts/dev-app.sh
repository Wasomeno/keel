#!/bin/bash
# Build the debug Keel and run it as target/KeelDebug.app (bundle id
# com.keel.dev.debug), in the background. As a bundle, screen tools can
# capture its window; the installed ~/Applications/Keel.app is untouched.
# Drive it with ui-dev.sock (see crates/ui/src/dev_control.rs).
#
#   scripts/dev-app.sh            # build, refresh the bundle, relaunch
#   scripts/dev-app.sh --no-build # refresh and relaunch only
set -euo pipefail
cd "$(dirname "$0")/.."
[ "${1:-}" = "--no-build" ] || cargo build -p keel
APP=target/KeelDebug.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
# The bundle must own its executable; a wrapper script that execs the debug
# binary leaves the window outside the bundle. An APFS clone costs no disk.
cp -c target/debug/keel "$APP/Contents/MacOS/.keel.new" 2>/dev/null || cp target/debug/keel "$APP/Contents/MacOS/.keel.new"
mv -f "$APP/Contents/MacOS/.keel.new" "$APP/Contents/MacOS/keel"
# Keel finds Pi at ../Resources/pi beside its binary.
ln -sfn ../../../debug/pi "$APP/Contents/Resources/pi"
ln -sfn ../../../debug/pi-packages "$APP/Contents/Resources/pi-packages"

# CEF framework and helpers
CEF_SRC="$(pwd)/.cef/cef_macos_aarch64/Chromium Embedded Framework.framework"
if [ -d "$CEF_SRC" ]; then
  ln -sfn "$CEF_SRC" "$APP/Contents/Frameworks/Chromium Embedded Framework.framework"
fi

HELPERS=(
  "Keel Debug Helper:com.keel.dev.debug.helper"
  "Keel Debug Helper (GPU):com.keel.dev.debug.helper.gpu"
  "Keel Debug Helper (Renderer):com.keel.dev.debug.helper.renderer"
  "Keel Debug Helper (Plugin):com.keel.dev.debug.helper.plugin"
  "Keel Debug Helper (Alerts):com.keel.dev.debug.helper.alerts"
)

for ENTRY in "${HELPERS[@]}"; do
  HNAME="${ENTRY%%:*}"
  HID="${ENTRY##*:}"
  HDIR="$APP/Contents/Frameworks/$HNAME.app"
  mkdir -p "$HDIR/Contents/MacOS"
  if [ -f target/debug/keel-helper ]; then
    cp -c target/debug/keel-helper "$HDIR/Contents/MacOS/.keel-helper.new" 2>/dev/null || cp target/debug/keel-helper "$HDIR/Contents/MacOS/.keel-helper.new"
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
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.keel.dev.debug</string>
  <key>CFBundleName</key><string>Keel Debug</string>
  <key>CFBundleDisplayName</key><string>Keel Debug</string>
  <key>CFBundleExecutable</key><string>keel</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
  $ICON_KEYS
</dict></plist>
PLIST
pkill -f 'KeelDebug.app/Contents/MacOS/keel|target/debug/keel' || true
sleep 1
# -g: launch behind the current app, so the user's screen stays theirs.
# Do not steal focus: open -g plus KEEL_NO_ACTIVATE skips cx.activate.
export KEEL_NO_ACTIVATE=1
open -g "$APP"
