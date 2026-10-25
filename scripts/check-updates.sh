#!/usr/bin/env bash
# Checks a built Tidy.app against the update contract (app-updates/CONTRACT.md).
#
#   scripts/check-updates.sh <path/Tidy.app> <path/tidyctl>
#
# Prints PASS or FAIL per check and exits 1 when any check fails. Reads files only: it never
# launches the app or the CLI, and needs no network.
#
#   framework  Contents/Frameworks/Sparkle.framework is there with its Versions/Current link
#   plist      SUPublicEDKey is the contract key; SUFeedURL, SUEnableAutomaticChecks,
#              SUAutomaticallyUpdate and SUScheduledCheckInterval are the contract values;
#              CFBundleShortVersionString is the VERSION file, CFBundleVersion is YYYYMMDDHHMM
#   app-link   otool -L of Contents/MacOS/tidy shows @rpath/Sparkle.framework/Versions/B/Sparkle
#              and the binary has the rpath @executable_path/../Frameworks
#   cli-link   otool -L of tidyctl shows no Sparkle
#   codesign   codesign --verify --deep --strict passes
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
APP_LOWER=tidy
BIN_NAME=tidy
# The contract's public EdDSA key and feed. CONTRACT.md is checked too when it is there.
PUBLIC_KEY='PMaW+8sZRzOkfIwy1KBGk0CHTVoN49FE9o8gUlNGdZc='
FEED="https://sdusteric.github.io/app-updates/$APP_LOWER/appcast.xml"
CONTRACT="$REPO/../app-updates/CONTRACT.md"
SPARKLE_LIB='@rpath/Sparkle.framework/Versions/B/Sparkle'

[ $# -eq 2 ] || { sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }
APP=$1
CLI=$2
PLIST="$APP/Contents/Info.plist"
PB=/usr/libexec/PlistBuddy

FAILED=0
pass() { printf 'PASS  %-10s %s\n' "$1" "$2"; }
fail() { printf 'FAIL  %-10s %s\n' "$1" "$2"; FAILED=1; }
plist_value() { "$PB" -c "Print :$1" "$PLIST" 2>/dev/null; }

# framework
fw="$APP/Contents/Frameworks/Sparkle.framework"
if [ -f "$fw/Versions/B/Sparkle" ] && [ -L "$fw/Versions/Current" ] && [ "$(readlink "$fw/Versions/Current")" = B ] &&
  [ -L "$fw/Sparkle" ] && [ -d "$fw/Versions/B/XPCServices/Installer.xpc" ] && [ -d "$fw/Versions/B/Updater.app" ]; then
  pass framework "$fw"
else
  fail framework "no complete Sparkle.framework (with its symlinks) in $APP/Contents/Frameworks"
fi

# plist
bad=""
key=$(plist_value SUPublicEDKey)
[ "$key" = "$PUBLIC_KEY" ] || bad="$bad [SUPublicEDKey=$key]"
if [ -f "$CONTRACT" ] && ! grep -qF "$PUBLIC_KEY" "$CONTRACT"; then
  bad="$bad [the key in this script is not the one in $CONTRACT]"
fi
v=$(plist_value SUFeedURL)
[ "$v" = "$FEED" ] || bad="$bad [SUFeedURL=$v]"
v=$(plist_value SUEnableAutomaticChecks)
[ "$v" = true ] || bad="$bad [SUEnableAutomaticChecks=$v]"
v=$(plist_value SUAutomaticallyUpdate)
[ "$v" = true ] || bad="$bad [SUAutomaticallyUpdate=$v]"
v=$(plist_value SUScheduledCheckInterval)
[ "$v" = 86400 ] || bad="$bad [SUScheduledCheckInterval=$v]"
want=$(tr -d '[:space:]' <"$REPO/VERSION" 2>/dev/null)
v=$(plist_value CFBundleShortVersionString)
[ -n "$want" ] && [ "$v" = "$want" ] || bad="$bad [CFBundleShortVersionString=$v, VERSION=$want]"
v=$(plist_value CFBundleVersion)
printf '%s' "$v" | grep -Eq '^20[0-9]{10}$' || bad="$bad [CFBundleVersion=$v is not YYYYMMDDHHMM]"
if [ -z "$bad" ]; then
  pass plist "SUPublicEDKey is the contract key; feed $FEED"
else
  fail plist "$bad"
fi

# app-link
bin="$APP/Contents/MacOS/$BIN_NAME"
if [ ! -f "$bin" ]; then
  fail app-link "no $bin"
elif ! otool -L "$bin" | grep -qF "$SPARKLE_LIB"; then
  fail app-link "otool -L $bin does not show $SPARKLE_LIB"
elif ! otool -l "$bin" | grep -A2 LC_RPATH | grep -qF '@executable_path/../Frameworks'; then
  fail app-link "$bin has no rpath @executable_path/../Frameworks"
else
  pass app-link "$bin links $SPARKLE_LIB"
fi

# cli-link
if [ ! -f "$CLI" ]; then
  fail cli-link "no $CLI"
elif otool -L "$CLI" | grep -qi sparkle; then
  fail cli-link "$CLI links Sparkle"
else
  pass cli-link "$CLI does not link Sparkle"
fi

# codesign
if out=$(codesign --verify --deep --strict --verbose=2 "$APP" 2>&1); then
  pass codesign "codesign --verify --deep --strict"
else
  fail codesign "$(printf '%s' "$out" | tail -n 3 | tr '\n' ' ')"
fi

exit $FAILED
