#!/usr/bin/env bash
# Captures Tidy demo scenarios without ever showing a window or taking focus.
#
# Usage: scripts/shots.sh [scenario ...]    (default: the five screens, in sidebar order:
#        checkup cleanup large memory security)
#        Scenarios: checkup cleanup cleanup-done cleanup-empty large large-not-opened
#        large-folders large-empty memory memory-tight memory-relaunch memory-critical security security-first security-items
#        cleanup-why system-data system-data-report checkup-trust cleanup-confirm history
#        cleanup-focus (the keyboard on a row checkbox, with the focus ring).
#        uninstaller, uninstaller-app, uninstaller-confirm (the Uninstaller list, an open app, its receipt).
#        Each one is captured in light and dark and written to $OUT/<scenario>-<light|dark>.png.
# Env:   TIDY_APP  bundle to launch (default target/bundle/Tidy.app)
#        OUT       where the PNGs go (default docs/app-shots)
#        SHOTS_PROBE=1  take no screenshot: launch, print the window id and geometry and the
#                       app's "demo window opened" log line, then quit. Works while the screen
#                       is locked. Not available together with SHOTS_RENDER.
#        SHOTS_RENDER=1 the app renders its own first frame offscreen (Metal) and writes the PNG,
#                       instead of `screencapture -l`. Works while the screen is locked, needs
#                       no Screen Recording permission, and always renders at 2x: the window
#                       opens outside every display, where gpui uses scale 2, and the app
#                       refuses to write a shot at any other scale. The PNG is 1800 x 1240 px.
#                       Needs the shot-export bundle, built with scripts/shots-bundle.sh
#                       (default TIDY_APP is then target/shots-build/bundle/Tidy.app, bundle id
#                       dev.sdusteric.tidy.shots). The PNG has no traffic lights and square
#                       corners; everything gpui draws is the same frame. This mode never
#                       looks for the window, so it does not use scripts/winid.swift.
#
# Scale: without SHOTS_RENDER, `screencapture -l` captures at the display's scale, so the
# script refuses to run unless the main display is 2x (exit 5): a 1x display would give
# half-size shots.
#
# How it stays invisible: each shot is one `open -g -n` launch of the bundle with
# TIDY_BACKGROUND=1, TIDY_HOME=<mktemp dir>, TIDY_SCAN_ROOT=<mktemp dir>/home and
# TIDY_DEMO=<scenario>. The app creates its window hidden, never activates, and paints exactly
# one frame. Without SHOTS_RENDER the window is found with CGWindowList (never NSWorkspace) and
# captured with `screencapture -l`. The frontmost app is recorded before and after every
# launch; if it changes, the script stops and exits 3.
#
# Rules: only launches go through `open -g`; every process is stopped by PID; the only thing
# deleted is the exact mktemp directory, and only when its path starts with the expected prefix.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
RENDER="${SHOTS_RENDER:-0}"
if [ "$RENDER" = 1 ]; then
  APP="${TIDY_APP:-$REPO/target/shots-build/bundle/Tidy.app}"
else
  APP="${TIDY_APP:-$REPO/target/bundle/Tidy.app}"
fi
BIN="$APP/Contents/MacOS/tidy"
OUT="${OUT:-$REPO/docs/app-shots}"
LOG_DIR="$REPO/target/shots"
LOG="$LOG_DIR/shots.log"
TMP_ROOT="${TMPDIR:-/tmp}"
TMP_ROOT="${TMP_ROOT%/}"
WINID="$TMP_ROOT/tidy-winid"
SRGB_ICC="/System/Library/ColorSync/Profiles/sRGB Profile.icc"
SCENARIOS=("$@")
[ ${#SCENARIOS[@]} -gt 0 ] || SCENARIOS=(checkup cleanup large memory security)

# The app maps an unknown TIDY_DEMO name to cleanup, so a typo here would shoot the wrong screen.
KNOWN_SCENARIOS=" checkup cleanup cleanup-done cleanup-empty large large-not-opened large-folders large-empty memory memory-tight memory-relaunch memory-critical security security-first security-items cleanup-why system-data system-data-report checkup-trust cleanup-confirm history cleanup-focus uninstaller uninstaller-app uninstaller-confirm "
for s in "${SCENARIOS[@]}"; do
  case "$KNOWN_SCENARIOS" in
    *" $s "*) ;;
    *) echo "unknown scenario $s; known:$KNOWN_SCENARIOS" >&2; exit 2 ;;
  esac
done

[ -d "$APP" ] || { echo "no bundle at $APP (run scripts/bundle.sh --release)" >&2; exit 1; }
[ -x "$BIN" ] || { echo "no binary at $BIN" >&2; exit 1; }
[ -f "$SRGB_ICC" ] || { echo "missing $SRGB_ICC" >&2; exit 1; }
grep -qa "TIDY_DEMO" "$BIN" || { echo "the bundle has no demo mode; rebuild it with scripts/bundle.sh --release" >&2; exit 1; }
PROBE="${SHOTS_PROBE:-0}"
if [ "$RENDER" = 1 ]; then
  grep -qa "TIDY_DEMO_SHOT" "$BIN" || { echo "$APP has no offscreen export; build it with scripts/shots-bundle.sh" >&2; exit 1; }
  if [ "$PROBE" = 1 ]; then
    echo "SHOTS_PROBE needs the window lookup, which SHOTS_RENDER does not do; use one or the other" >&2
    exit 2
  fi
fi
# Our processes are the ones running this bundle's binary. Match on its path (case-insensitive:
# the repo path may be spelled Projects or projects), so an installed Tidy.app the user runs
# elsewhere neither blocks the shots nor gets mistaken for ours. It is never touched.
ours() { pgrep -if -- "$BIN" || true; }
if [ -n "$(ours)" ]; then
  echo "$APP is already running; quit it first (this script never touches it)" >&2
  exit 1
fi
if [ "$RENDER" != 1 ]; then
  if [ ! -x "$WINID" ] || [ "$REPO/scripts/winid.swift" -nt "$WINID" ]; then
    swiftc -O "$REPO/scripts/winid.swift" -o "$WINID"
  fi
  # screencapture cannot read any window while the screen is locked (the frontmost app is then
  # loginwindow). Refuse before launching anything.
  if [ "$PROBE" != 1 ] && "$WINID" --locked; then
    echo "the screen is locked, so windows cannot be captured; unlock it and run again," >&2
    echo "or use SHOTS_RENDER=1 (see the header)" >&2
    exit 4
  fi
  # The shots are 2x; a screencapture is at the display's scale.
  if [ "$PROBE" != 1 ]; then
    DISPLAY_SCALE="$("$WINID" --scale)"
    if [ "$DISPLAY_SCALE" != 2 ]; then
      echo "the main display is ${DISPLAY_SCALE}x, but the shots are 2x, so screencapture" >&2
      echo "would give shots of the wrong size. Use SHOTS_RENDER=1 (renders at 2x on any" >&2
      echo "display, after scripts/shots-bundle.sh) or run on a 2x display." >&2
      exit 5
    fi
  fi
fi
mkdir -p "$OUT" "$LOG_DIR"
: > "$LOG"

# The frontmost app as "name (asn)", on one line.
front() {
  local asn line name
  asn="$(lsappinfo front 2>/dev/null || true)"
  line="$(lsappinfo info -only name "$asn" 2>/dev/null | head -1 || true)"
  if [[ "$line" =~ =\"([^\"]*)\" ]]; then
    name="${BASH_REMATCH[1]}"
  elif [[ "$line" =~ ^\"([^\"]*)\" ]]; then
    name="${BASH_REMATCH[1]}"
  else
    name="unknown"
  fi
  echo "$name ($asn)"
}

PID=""
HOME_DIR=""

stop_app() {
  if [ -n "$PID" ]; then
    kill -TERM "$PID" 2>/dev/null || true
    for _ in $(seq 1 25); do
      kill -0 "$PID" 2>/dev/null || break
      sleep 0.2
    done
    if kill -0 "$PID" 2>/dev/null; then kill -KILL "$PID" 2>/dev/null || true; fi
    PID=""
  fi
}

remove_home() {
  case "$HOME_DIR" in
    "$TMP_ROOT"/tidy-shots.?*) if [ -d "$HOME_DIR" ]; then rm -rf -- "$HOME_DIR"; fi ;;
  esac
  HOME_DIR=""
}

cleanup() {
  stop_app
  remove_home
}
trap cleanup EXIT

FAILED=0

shoot() { # scenario appearance
  local scn="$1" mode="$2" want="Tidy" id="" before after wid_line="" w h n wscale
  HOME_DIR="$(mktemp -d "$TMP_ROOT/tidy-shots.XXXXXX")"
  mkdir -p "$HOME_DIR/home"
  local shot_env=()
  if [ "$RENDER" = 1 ]; then
    shot_env=(--env TIDY_DEMO_SHOT="$HOME_DIR/capture.png")
  fi
  before="$(front)"

  open -g -n \
    --env TIDY_BACKGROUND=1 \
    --env TIDY_HOME="$HOME_DIR" \
    --env TIDY_SCAN_ROOT="$HOME_DIR/home" \
    --env TIDY_DEMO="$scn" \
    --env TIDY_APPEARANCE="$mode" \
    --env TIDY_DEMO_EXIT_SECS=30 \
    ${shot_env[@]+"${shot_env[@]}"} \
    "$APP"

  # Find our process (the only tidy). Without SHOTS_RENDER, also its window by name. With it,
  # wait for the PNG instead. Up to 15 s.
  for _ in $(seq 1 75); do
    n="$(ours | wc -l | tr -d ' ')"
    if [ "$n" -gt 1 ]; then
      echo "more than one tidy process appeared; stopping" >&2
      exit 2
    fi
    if [ "$n" = 1 ]; then
      PID="$(ours | head -1)"
      if [ "$RENDER" != 1 ]; then
        wid_line="$("$WINID" "$PID" --all 2>/dev/null | grep -F "name=$want" | head -1 || true)"
        if [ -n "$wid_line" ]; then id="${wid_line%% *}"; break; fi
      fi
    fi
    # The render mode needs no window id. The app may quit right after it writes the PNG, so
    # the PNG, not the process, ends the wait.
    if [ "$RENDER" = 1 ] && [ -s "$HOME_DIR/capture.png" ]; then break; fi
    sleep 0.2
  done

  local out="$OUT/$scn-$mode.png" size="missing"
  if [ "$RENDER" != 1 ] && [ -z "$id" ]; then
    echo "no \"$want\" window for $scn/$mode" >&2
    FAILED=1
  else
    if [ "$RENDER" != 1 ]; then
      sleep 1.2 # the first frame is painted at creation; this is only a margin
    fi
    if [ "$PROBE" = 1 ]; then
      echo "probe $scn/$mode: $wid_line"
      grep -h "demo window" "$HOME_DIR"/logs/tidy.log 2>/dev/null | tail -1 || true
    fi
    # Capture into the temp home first, so a failed capture never leaves a stale shot behind.
    if [ "$PROBE" = 1 ]; then
      :
    elif [ "$RENDER" = 1 ]; then
      # The app writes capture.png and quits.
      if [ -s "$HOME_DIR/capture.png" ]; then
        # The frame holds the raw values gpui draws, which the window server shows as sRGB.
        # Tag them sRGB; the conversion below is then a no-op.
        sips --embedProfile "$SRGB_ICC" "$HOME_DIR/capture.png" >/dev/null
      else
        grep -h "demo shot" "$HOME_DIR"/logs/tidy.log 2>/dev/null | tail -1 >&2 || true
      fi
    else
      # A window on a display of another scale (a second monitor) would capture at that scale.
      wscale="$(printf '%s' "$wid_line" | sed -n 's/.* scale=\([^ ]*\) .*/\1/p')"
      if [ "$wscale" = 2 ]; then
        screencapture -x -o -l "$id" "$HOME_DIR/capture.png" || true
      else
        echo "the $scn/$mode window is on a ${wscale:-unknown}x display, not 2x; no capture" >&2
      fi
    fi
    if [ "$PROBE" = 1 ]; then
      size="probe"
    elif [ -s "$HOME_DIR/capture.png" ]; then
      # screencapture tags the PNG with the display's profile (Display P3). The shots are sRGB,
      # so convert, or every color reads off when the shots are compared.
      sips --matchTo "$SRGB_ICC" "$HOME_DIR/capture.png" >/dev/null
      mv -f "$HOME_DIR/capture.png" "$out"
      w="$(sips -g pixelWidth "$out" | awk '/pixelWidth/{print $2}')"
      h="$(sips -g pixelHeight "$out" | awk '/pixelHeight/{print $2}')"
      size="${w}x${h}"
    else
      echo "capture missing for $scn/$mode" >&2
      FAILED=1
    fi
  fi

  after="$(front)"
  echo "$scn $mode ${PID:-none} before=$before after=$after size=$size" >> "$LOG"
  local launched_pid="$PID"
  stop_app
  remove_home

  if [ "$before" != "$after" ]; then
    echo "frontmost app changed during $scn/$mode: $before -> $after (pid ${launched_pid:-none})" >&2
    exit 3
  fi
  echo "ok $scn-$mode $size"
}

for s in "${SCENARIOS[@]}"; do
  for a in light dark; do
    shoot "$s" "$a"
  done
done

if [ -n "$(ours)" ]; then
  echo "$APP is still running after the shots" >&2
  FAILED=1
fi
echo "log: $LOG"
exit "$FAILED"
