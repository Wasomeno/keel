#!/bin/bash
# Build the debug Keel and run it as target/KeelDebug.app (bundle id
# com.keel.dev.debug), in the background. As a bundle, screen tools can
# capture its window; the installed ~/Applications/Keel.app is untouched.
# Drive it with ui-dev.sock (see crates/ui/src/dev_control.rs).
#
#   scripts/dev-app.sh            # build, refresh the bundle, relaunch in foreground
#   scripts/dev-app.sh --no-build # refresh and relaunch only
#   scripts/dev-app.sh --bg       # launch in background without stealing focus
set -euo pipefail
cd "$(dirname "$0")/.."

NO_BUILD=0
BG=0
for arg in "$@"; do
  case "$arg" in
    --no-build) NO_BUILD=1 ;;
    --bg) BG=1 ;;
    *) echo "Unknown flag: $arg" >&2; exit 2 ;;
  esac
done

[ "$NO_BUILD" -eq 1 ] || cargo build -p keel
APP=target/KeelDebug.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
# The bundle must own its executable; a wrapper script that execs the debug
# binary leaves the window outside the bundle. An APFS clone costs no disk.
cp -c target/debug/keel "$APP/Contents/MacOS/.keel.new" 2>/dev/null || cp target/debug/keel "$APP/Contents/MacOS/.keel.new"
mv -f "$APP/Contents/MacOS/.keel.new" "$APP/Contents/MacOS/keel"

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
PATTERN='KeelDebug.app/Contents/MacOS/keel|target/debug/keel'
pkill -f "$PATTERN" || true

# Wait for the old instance (and its CEF teardown) to actually exit so `open`
# doesn't collide with a dying process and silently no-op.
for _ in $(seq 1 50); do
  if ! pgrep -f "$PATTERN" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
if pgrep -f "$PATTERN" >/dev/null 2>&1; then
  pkill -9 -f "$PATTERN" || true
  sleep 0.2
fi

APP_ABS="$(pwd)/$APP"
if [ "$BG" -eq 1 ]; then
  # -g: launch behind the current app, so the user's screen stays theirs.
  export KEEL_NO_ACTIVATE=1
  open -g -n "$APP_ABS"
else
  unset KEEL_NO_ACTIVATE || true
  open -n "$APP_ABS"
fi

# Confirm the new instance actually came up.
for _ in $(seq 1 30); do
  if pgrep -f 'KeelDebug.app/Contents/MacOS/keel' >/dev/null 2>&1; then
    exit 0
  fi
  sleep 0.1
done
echo "error: KeelDebug.app did not start" >&2
exit 1
