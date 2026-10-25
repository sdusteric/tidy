#!/usr/bin/env bash
# Releases Tidy: runs the gates, builds the signed release bundle, checks it against the update
# contract and hands it to the app-updates publisher.
#
#   scripts/release.sh [--notes <file.md>] [--dry-run <dir>]
#
#   --notes <file.md>  release notes, passed to publish.sh
#   --dry-run <dir>    passed to publish.sh, which then writes what it would publish into <dir>
#                      and uploads nothing; notarize.sh prints its commands and does not submit
#
# Steps, each one stops the release when it fails:
#   1. scripts/accept.sh offline with ACCEPT_SKIP_BUNDLE=1 (and never ACCEPT_SKIP_GATES): AC1
#      (build, fmt, clippy, all default tests) and AC2 to AC9 without the screenshots
#   2. scripts/bundle.sh --release, and a release tidyctl
#   3. scripts/check-updates.sh on both: Sparkle embedded and signed, the contract key and
#      feed, only the app links Sparkle, codesign --verify --deep --strict
#      and scripts/check-signing.sh on the app: hardened runtime, entitlements, one team
#   4. ../app-updates/tools/notarize.sh: with a Developer ID Application certificate in the
#      keychain and the notarytool profile sdusteric-notary, notarize and staple the app; with
#      the Apple Development fallback, warn and go on (app-updates/docs/notarization.md)
#   5. ../app-updates/tools/publish.sh --app tidy --bundle <Tidy.app> [--notes] [--dry-run]
#      (--zip <the stapled app, zipped after stapling> when it was notarized)
#
# The version is the VERSION file; the build number is the UTC time of step 2. Never launches or
# installs the app. Honors CARGO_TARGET_DIR (the bundle is <target dir>/bundle/Tidy.app).
#
# Environment:
#   APP_UPDATES_PUBLISH  the publish.sh to call (default ../app-updates/tools/publish.sh)
#   APP_UPDATES_NOTARIZE the notarize.sh to call (default: next to publish.sh)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
PUBLISH="${APP_UPDATES_PUBLISH:-$REPO/../app-updates/tools/publish.sh}"
NOTARIZE="${APP_UPDATES_NOTARIZE:-$(dirname "$PUBLISH")/notarize.sh}"

usage() {
  sed -n '2,29p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

NOTES=""
DRY_RUN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --notes)
      [ $# -ge 2 ] || usage
      NOTES=$2
      shift 2
      ;;
    --dry-run)
      [ $# -ge 2 ] || usage
      DRY_RUN=$2
      shift 2
      ;;
    -h | --help) usage ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      ;;
  esac
done

if [ -n "$NOTES" ]; then
  [ -f "$NOTES" ] || { echo "no notes file $NOTES" >&2; exit 1; }
  NOTES="$(cd "$(dirname "$NOTES")" && pwd -P)/$(basename "$NOTES")"
fi
[ -x "$PUBLISH" ] || { echo "publish.sh not found or not executable: $PUBLISH" >&2; exit 1; }
[ -x "$NOTARIZE" ] || { echo "notarize.sh not found or not executable: $NOTARIZE" >&2; exit 1; }

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$REPO/$TARGET_DIR" ;; esac
APP="$TARGET_DIR/bundle/Tidy.app"
CLI="$TARGET_DIR/release/tidyctl"

echo "== 1. gates (scripts/accept.sh offline, bundle checks follow below)"
env -u ACCEPT_SKIP_GATES ACCEPT_SKIP_BUNDLE=1 "$REPO/scripts/accept.sh" offline

echo "== 2. release bundle and tidyctl"
"$REPO/scripts/bundle.sh" --release
(cd "$REPO" && cargo build -p tidy-cli --release)

echo "== 3. update contract"
"$REPO/scripts/check-updates.sh" "$APP" "$CLI"
"$REPO/scripts/check-signing.sh" "$APP"

echo "== 4. notarize"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/tidy-release.XXXXXX")"
trap '[ -n "${WORK:-}" ] && [ -d "$WORK" ] && rm -rf "$WORK"' EXIT
FINAL_ZIP="$WORK/final.zip"
nargs=(--bundle "$APP" --final-zip "$FINAL_ZIP")
[ -z "$DRY_RUN" ] || nargs+=(--dry-run)
rc=0
"$NOTARIZE" "${nargs[@]}" || rc=$?

echo "== 5. publish $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist") ($(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist"))"
args=(--app tidy --bundle "$APP")
[ -n "$NOTES" ] && args+=(--notes "$NOTES")
[ -n "$DRY_RUN" ] && args+=(--dry-run "$DRY_RUN")
case "$rc" in
  0) args+=(--zip "$FINAL_ZIP") ;; # the stapled app, zipped after stapling: the Sparkle signature covers it
  10) ;;                           # not notarized (fallback certificate or dry run)
  *) exit "$rc" ;;
esac
"$PUBLISH" "${args[@]}"
