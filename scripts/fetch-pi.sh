#!/bin/bash
# Put the Pi release Keel runs, and the Pi packages it ships, into DEST:
#
#   DEST/pi/pi                                 Pi's standalone build
#   DEST/pi-packages/pi-commandcode-provider   Command Code sign-in and models
#   DEST/pi-packages/pi-web-access             web search and page fetching
#   DEST/pi-packages/pi-subagents              subagent delegation
#
# The engine looks for Pi at Contents/Resources/pi in Keel.app (bundle-mac.sh
# passes that folder) and at pi/ beside the binary, so for a dev build:
#
#   scripts/fetch-pi.sh target/debug
#
# Versions and checksums are pinned. PI_VERSION must match `PI_VERSION` in
# crates/harness/src/keel.rs. Downloads are cached in target/pi-cache.
set -euo pipefail
cd "$(dirname "$0")/.."
DEST=${1:?usage: scripts/fetch-pi.sh DEST}

PI_VERSION=1.3.0
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) PI_ASSET=pi-darwin-arm64.tar.gz PI_SHA=1f9ae61cdeae04dd68caf1a1e8ab8ec9b292062f6ccb3252e0a9cbe93b5b16f1 ;;
  Darwin-x86_64) PI_ASSET=pi-darwin-x64.tar.gz PI_SHA=9ff562e2e439f50971b1f6bd9b62b30946600d9e9baa4ea19635cf630a553066 ;;
  Linux-x86_64) PI_ASSET=pi-linux-x64.tar.gz PI_SHA=24f5be328b66d4da36876296989501974f43ab8434fbb57c35c523a3ea505ded ;;
  Linux-aarch64) PI_ASSET=pi-linux-arm64.tar.gz PI_SHA=7fd2ea14fca390ebf9f47438baaef07c4b3d13db7cc8e459831118391b7ea65b ;;
  *) echo "no Pi build for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
CC_VERSION=0.7.8
CC_SHA=28aecd5c1afce04f615e6957a7caf04839131a3766b0432daabaaefde85073dd
# Packages with npm dependencies, as name@version. npm checks each tarball
# against the registry's integrity hash.
NPM_PACKAGES=(pi-web-access@0.42.0 pi-subagents@0.79.1)

CACHE=target/pi-cache
mkdir -p "$CACHE"

# fetch URL FILE SHA256: download once, then check the checksum every time.
fetch() {
  if [ ! -f "$2" ]; then
    curl -fsSL -o "$2.part" "$1"
    mv "$2.part" "$2"
  fi
  echo "$3  $2" | shasum -a 256 -c --quiet - || { rm -f "$2"; echo "checksum mismatch: $1" >&2; exit 1; }
}

PI_TAR="$CACHE/pi-$PI_VERSION-$PI_ASSET"
fetch "https://github.com/earendil-works/pi/releases/download/v$PI_VERSION/$PI_ASSET" "$PI_TAR" "$PI_SHA"
CC_TAR="$CACHE/pi-commandcode-provider-$CC_VERSION.tgz"
fetch "https://registry.npmjs.org/pi-commandcode-provider/-/pi-commandcode-provider-$CC_VERSION.tgz" "$CC_TAR" "$CC_SHA"

# Replace, not merge: no file from an older version is left behind.
rm -rf "$DEST/pi" "$DEST/pi-packages"
mkdir -p "$DEST/pi-packages/pi-commandcode-provider"
tar xzf "$PI_TAR" -C "$DEST"
tar xzf "$CC_TAR" -C "$DEST/pi-packages/pi-commandcode-provider" --strip-components 1

# Each into its own folder with its runtime dependencies. Pi itself (the
# peers) comes from the host, so peers are skipped, as are install scripts.
# ponytail: dependencies float within each package's ranges; commit lockfiles if builds must be byte-identical.
for spec in "${NPM_PACKAGES[@]}"; do
  name=${spec%@*}
  dir="$DEST/pi-packages/$name"
  mkdir -p "$dir"
  tgz=$(cd "$dir" && npm pack --silent "$spec")
  tar xzf "$dir/$tgz" -C "$dir" --strip-components 1
  rm "$dir/$tgz"
  (cd "$dir" && npm install --omit=dev --omit=peer --legacy-peer-deps --ignore-scripts --no-audit --no-fund --no-package-lock --silent)
done
echo "Pi $("$DEST/pi/pi" --version), pi-commandcode-provider $CC_VERSION and ${NPM_PACKAGES[*]} in $DEST"
