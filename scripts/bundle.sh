#!/usr/bin/env bash
# Build, sign and optionally install Tidy.app. Usage: scripts/bundle.sh [--release] [--install]
# Never launches the app.
#
# Test overrides (all optional):
#   TIDY_BUNDLE_BIN  use this binary instead of running cargo build
#   INSTALL_DIR      install target dir (default: $HOME/Applications)
#   SIGN_ID          signing identity SHA-1 or name (default: the Developer ID Application cert of team
#                    8N9KUZLT8N when the keychain has one, else the Apple Development cert)
#   TIDY_BUNDLE_ID   CFBundleIdentifier (default dev.sdusteric.tidy). scripts/shots-bundle.sh
#                    gives the screenshot-only bundle its own id, so LaunchServices never
#                    confuses it with an installed Tidy.
#   CARGO_TARGET_DIR cargo target dir (default: target in the repo). The bundle goes to
#                    <target dir>/bundle/Tidy.app.
#   SPARKLE_FRAMEWORK the Sparkle.framework to embed (default: the app-updates repo next to
#                    this one, ../app-updates/vendor/Sparkle-2.10.0/Sparkle.framework). The
#                    tidy-app build script links the same path.
#
# Versions: CFBundleShortVersionString is the VERSION file at the repo root, CFBundleVersion is
# the UTC build time YYYYMMDDHHMM (app-updates/CONTRACT.md).
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
# Signing identity. SIGN_ID (a SHA-1 or a name) overrides. Otherwise: the Developer ID Application
# certificate of team 8N9KUZLT8N when it is in the keychain, else the Apple Development
# certificate. Picked by SHA-1, not name: several certs can share the CN and some are expired.
TEAM_ID=8N9KUZLT8N
APPLE_DEV_SHA=1EDDD87FAAACC222585E8C76E983DB798EC38331
IDENTITIES="$(security find-identity -v -p codesigning)"
if [ -z "${SIGN_ID:-}" ]; then
  SIGN_ID="$(awk -v team="($TEAM_ID)" '/"Developer ID Application: / && index($0, team) { print $2; exit }' <<<"$IDENTITIES")"
  SIGN_ID="${SIGN_ID:-$APPLE_DEV_SHA}"
fi
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$REPO/$TARGET_DIR" ;; esac
OUT="$TARGET_DIR/bundle/Tidy.app"

# Short version from the VERSION file (semver), build number from the UTC clock.
[ -f "$REPO/VERSION" ] || { echo "missing $REPO/VERSION" >&2; exit 1; }
VERSION="$(tr -d '[:space:]' < "$REPO/VERSION")"
printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "VERSION is not semver: $VERSION" >&2; exit 1; }
BUILD="$(date -u +%Y%m%d%H%M)"

# Sparkle.framework, embedded in Contents/Frameworks.
SPARKLE="${SPARKLE_FRAMEWORK:-$REPO/../app-updates/vendor/Sparkle-2.10.0/Sparkle.framework}"
[ -f "$SPARKLE/Versions/B/Sparkle" ] || {
  echo "Sparkle.framework not found at $SPARKLE (check out app-updates next to this repo or set SPARKLE_FRAMEWORK)" >&2
  exit 1
}

# Check the signing identity before spending time on a build.
SIGN_LINE="$(grep -F -- "$SIGN_ID" <<<"$IDENTITIES" | head -n 1 || true)"
if [ -z "$SIGN_LINE" ]; then
  echo "signing identity $SIGN_ID is not valid; renew the Apple Development certificate in Xcode" >&2
  exit 1
fi
# A secure timestamp needs the network and Apple's server; notarization requires it. The Apple
# Development fallback skips it so a build works offline.
case "$SIGN_LINE" in
  *'"Developer ID Application: '*) TIMESTAMP=--timestamp ;;
  *) TIMESTAMP=--timestamp=none ;;
esac
echo "signing with:${SIGN_LINE#*)}"

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
mkdir -p "$OUT/Contents/MacOS" "$OUT/Contents/Resources" "$OUT/Contents/Frameworks"
cp "$BIN" "$OUT/Contents/MacOS/$BIN_NAME"
# ditto keeps the framework's symlinks (Versions/Current, the top-level links).
ditto --norsrc --noextattr "$SPARKLE" "$OUT/Contents/Frameworks/Sparkle.framework"

# Info.plist
sed -e "s|@BUNDLE_ID@|$BUNDLE_ID|" -e "s|@VERSION@|$VERSION|" -e "s|@BUILD@|$BUILD|" \
  "$REPO/resources/Info.plist.in" > "$OUT/Contents/Info.plist"
# The tidy:// scheme belongs to the real app only: a bundle with another id (the screenshot
# bundle) must never be where macOS sends tidy:// links.
if [ "$BUNDLE_ID" != "dev.sdusteric.tidy" ]; then
  /usr/libexec/PlistBuddy -c 'Delete :CFBundleURLTypes' "$OUT/Contents/Info.plist"
fi
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

# Sign inside out with one identity, all with the hardened runtime: Sparkle's helpers, then the
# framework, then the app. Sparkle's parts get no entitlements; Autoupdate's application-identifier
# is dropped on purpose, since it would need a provisioning profile. No --deep.
sign() { codesign --force --sign "$SIGN_ID" "$TIMESTAMP" --options runtime "$@"; }
FW="$OUT/Contents/Frameworks/Sparkle.framework"
for part in \
  "$FW/Versions/B/XPCServices/Installer.xpc" \
  "$FW/Versions/B/XPCServices/Downloader.xpc" \
  "$FW/Versions/B/Updater.app" \
  "$FW/Versions/B/Autoupdate" \
  "$FW"; do
  sign "$part"
done
# The app, with resources/Tidy.entitlements. The designated requirement keys on the bundle id and
# the certificate, so TCC grants survive rebuilds with the same certificate (they reset once when
# the certificate changes).
sign --entitlements "$REPO/resources/Tidy.entitlements" "$OUT"
codesign --verify --deep --strict --verbose=2 "$OUT"

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
