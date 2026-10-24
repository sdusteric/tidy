#!/usr/bin/env bash
# Build, sign and optionally install Tidy.app. Usage: scripts/bundle.sh [--release] [--install]
# Never launches the app.
#
# Test overrides (all optional):
#   TIDY_BUNDLE_BIN  use this binary instead of running cargo build
#   INSTALL_DIR      install target dir (default: $HOME/Applications)
#   SIGN_ID          signing identity SHA-1 (default: the Apple Development cert)
#   TIDY_BUNDLE_ID   CFBundleIdentifier (default dev.sdusteric.tidy). scripts/shots-bundle.sh
#                    gives the screenshot-only bundle its own id, so LaunchServices never
#                    confuses it with an installed Tidy.
#   CARGO_TARGET_DIR cargo target dir (default: target in the repo). The bundle goes to
#                    <target dir>/bundle/Tidy.app.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"

PROFILE=debug
INSTALL=0
for a in "$@"; do
  case "$a" in
    --release) PROFILE=release ;;
    --install) INSTALL=1 ;;
    *) echo "unknown argument: $a" >&2; exit 2 ;;
  esac
done

BIN_NAME=tidy
BUNDLE_ID="${TIDY_BUNDLE_ID:-dev.sdusteric.tidy}"
# SHA-1 of the cert, not the name: three certs share the CN on this Mac and two are expired.
SIGN_ID="${SIGN_ID:-1EDDD87FAAACC222585E8C76E983DB798EC38331}"
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$REPO/$TARGET_DIR" ;; esac
OUT="$TARGET_DIR/bundle/Tidy.app"

# Version from [workspace.package] in the root Cargo.toml.
VERSION="$(awk '/^\[workspace\.package\]/{s=1;next} /^\[/{s=0} s && $1=="version"{gsub(/"/,"",$3); print $3; exit}' "$REPO/Cargo.toml")"
[ -n "$VERSION" ] || { echo "could not read version from Cargo.toml" >&2; exit 1; }
BUILD="$(date +%Y%m%d%H%M)"

# Check the signing identity before spending time on a build.
if ! security find-identity -v -p codesigning | grep -q "$SIGN_ID"; then
  echo "signing identity not valid; renew the Apple Development certificate in Xcode" >&2
  exit 1
fi

if [ -n "${TIDY_BUNDLE_BIN:-}" ]; then
  BIN="$TIDY_BUNDLE_BIN"
else
  if [ "$PROFILE" = release ]; then
    (cd "$REPO" && cargo build -p tidy-app --release)
  else
    (cd "$REPO" && cargo build -p tidy-app)
  fi
  BIN="$TARGET_DIR/$PROFILE/$BIN_NAME"
fi
[ -x "$BIN" ] || { echo "missing binary $BIN" >&2; exit 1; }

# Only ever delete the bundle dir inside the target dir.
case "$OUT" in */bundle/Tidy.app) ;; *) echo "refusing odd output path $OUT" >&2; exit 1 ;; esac
if [ -d "$OUT" ]; then rm -rf "$OUT"; fi
mkdir -p "$OUT/Contents/MacOS" "$OUT/Contents/Resources"
cp "$BIN" "$OUT/Contents/MacOS/$BIN_NAME"

# Info.plist
sed -e "s|@BUNDLE_ID@|$BUNDLE_ID|" -e "s|@VERSION@|$VERSION|" -e "s|@BUILD@|$BUILD|" \
  "$REPO/resources/Info.plist.in" > "$OUT/Contents/Info.plist"
printf 'APPL????' > "$OUT/Contents/PkgInfo"
plutil -lint "$OUT/Contents/Info.plist"

# Icon: resources/AppIcon.png if present, else generated.
WORK="$(mktemp -d)"
trap '[ -n "${WORK:-}" ] && [ -d "$WORK" ] && rm -rf "$WORK"' EXIT
PNG="$REPO/resources/AppIcon.png"
if [ ! -f "$PNG" ]; then
  swift "$SCRIPT_DIR/make-icon.swift" "$WORK/icon-1024.png"
  PNG="$WORK/icon-1024.png"
fi
SET="$WORK/AppIcon.iconset"
mkdir "$SET"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$PNG" --out "$SET/icon_${s}x${s}.png" >/dev/null
  sips -z "$((s * 2))" "$((s * 2))" "$PNG" --out "$SET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o "$OUT/Contents/Resources/AppIcon.icns"

# Sign: Apple Development cert, no hardened runtime, no entitlements, no --deep.
# --timestamp=none needs no network. The designated requirement keys on bundle id and cert CN,
# so TCC grants survive rebuilds.
codesign --force --sign "$SIGN_ID" --timestamp=none "$OUT"
codesign --verify --strict --verbose=2 "$OUT"

if [ "$INSTALL" = 1 ]; then
  DEST_DIR="${INSTALL_DIR:-$HOME/Applications}"
  DEST="$DEST_DIR/Tidy.app"
  mkdir -p "$DEST_DIR"
  # Unregister the build copy so LaunchServices does not offer two Tidy.app's.
  "$LSREGISTER" -u "$OUT" 2>/dev/null || true
  if [ -d "$DEST" ]; then rm -rf "$DEST"; fi
  ditto "$OUT" "$DEST"
  "$LSREGISTER" -f "$DEST"
  echo "installed $DEST"
else
  echo "built $OUT"
fi
