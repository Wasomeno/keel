#!/bin/bash
# Put the Pi release Keel runs, and the Pi packages it ships, into DEST:
#
#   DEST/pi/pi                                 Pi's standalone build
#   DEST/pi-packages/pi-commandcode-provider   Command Code sign-in and models
#   DEST/pi-packages/pi-web-access             web search and page fetching
#   DEST/pi-packages/pi-subagents              subagent delegation
#   DEST/pi-packages/pi-antigravity-bridge     Google Antigravity models (antigravity/*)
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

PI_VERSION=1.5.0
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) PI_ASSET=pi-darwin-arm64.tar.gz PI_SHA=a9bf0d82d58d2a7d337f4ae4171b5fc9ebffe42509527a831364c4b65adf2cdf ;;
  Darwin-x86_64) PI_ASSET=pi-darwin-x64.tar.gz PI_SHA=f41b26cd9a1754abe259e05a421fb33b00936779cb0783c893a6389d1bd2ee0d ;;
  Linux-x86_64) PI_ASSET=pi-linux-x64.tar.gz PI_SHA=df870ba0e344cb7b3358d8d8f19397b8b8d225a8669784736ad68a5c2169d53b ;;
  Linux-aarch64) PI_ASSET=pi-linux-arm64.tar.gz PI_SHA=5d5711b48600756473682e86360b026b11abf7591e5d112eb411d4da2c100d9a ;;
  *) echo "no Pi build for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
CC_VERSION=0.7.8
CC_SHA=28aecd5c1afce04f615e6957a7caf04839131a3766b0432daabaaefde85073dd
# Packages with npm dependencies, as name@version. npm checks each tarball
# against the registry's integrity hash. A scoped package lands in a folder
# named without its scope, since Keel registers each pi-packages/ folder.
NPM_PACKAGES=(pi-web-access@0.42.0 pi-subagents@0.79.1 @estebanforge/pi-antigravity-bridge@1.9.2)

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

# Each into its own folder with its runtime dependencies, then patched. Pi itself (the
# peers) comes from the host, so peers are skipped, as are install scripts.
# ponytail: dependencies float within each package's ranges; commit lockfiles if builds must be byte-identical.
for spec in "${NPM_PACKAGES[@]}"; do
  name=${spec%@*}
  dir="$DEST/pi-packages/${name##*/}"
  mkdir -p "$dir"
  tgz=$(cd "$dir" && npm pack --silent "$spec")
  tar xzf "$dir/$tgz" -C "$dir" --strip-components 1
  rm "$dir/$tgz"
  # Keel's fixes to a package, as scripts/pi-patches/<folder>.patch. A patch
  # that no longer applies fails the fetch, so a version bump re-checks it.
  patch_file="scripts/pi-patches/${name##*/}.patch"
  if [ -f "$patch_file" ]; then
    patch -d "$dir" -p1 --forward --silent < "$patch_file"
  fi
  (cd "$dir" && npm install --omit=dev --omit=peer --legacy-peer-deps --ignore-scripts --no-audit --no-fund --no-package-lock --silent)
done
echo "Pi $("$DEST/pi/pi" --version), pi-commandcode-provider $CC_VERSION and ${NPM_PACKAGES[*]} in $DEST"
