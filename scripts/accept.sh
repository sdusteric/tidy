#!/usr/bin/env bash
# Tidy milestone 1 acceptance runner.
#
#   scripts/accept.sh offline
#
# Prints PASS, FAIL or SKIP for each criterion id (AC1 to AC5) and exits nonzero on any FAIL.
# The criteria are in docs/superpowers/specs/2026-10-23-tidy-design.md, section 13.
#
#   AC1  gates: build, fmt, clippy, tests
#   AC2  tidyctl scan on the fixture home equals expected-plan.json
#   AC3  tidyctl clean --safe moves exactly the safe set; without TIDY_HOME it exits 5
#   AC4  tidyctl large equals expected-large.json (items, --not-opened, --folders)
#   AC5  signed bundle, Info.plist, --self-test, offscreen screenshots at 1800 x 1240 px
#
# Environment:
#   ACCEPT_SKIP_BUNDLE=1  skip AC5 (it signs with the keychain and runs two release builds).
#                         AC5 then shows as SKIP.
#   ACCEPT_SKIP_GATES=1   skip AC1 (the full build and test run), for quick runs while
#                         developing. AC1 then shows as SKIP. tidyctl and tidy-fixture-home are
#                         still built, because the other criteria need them.
#
# Safety: every tidyctl run sets TIDY_HOME and TIDY_SCAN_ROOT to a fixture inside a fresh
# folder under $TMPDIR, so the real home folder, the real Trash and the real Tidy folders are
# never read or written. (The fixture cannot live under target/accept: the repo is under
# ~/Documents, which tidy-fixture-home refuses.) The app is never started except for
# `tidy --self-test` and the offscreen screenshots of scripts/shots.sh with SHOTS_RENDER=1,
# both with TIDY_BACKGROUND=1 and a temporary TIDY_HOME. Nothing is installed. Every process
# this script starts is stopped by PID when it exits. Logs are in target/accept/<timestamp>.

# shellcheck source=scripts/accept-lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/accept-lib.sh"

usage() {
  sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
}

GROUP=${1:-}
case "$GROUP" in
  offline) ;;
  *)
    usage
    exit 2
    ;;
esac

clear_tidy_env

echo "Tidy acceptance, group: $GROUP"
echo "Run folder: $RUN"

# Keep the Mac from idle sleep while the run lasts. caffeinate cannot prevent the sleep of a
# closed laptop on battery: keep the lid open or the power connected.
/usr/bin/caffeinate -i -w $$ &
track_pid $!

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$REPO/$TARGET_DIR" ;; esac
TIDYCTL="$TARGET_DIR/debug/tidyctl"
FIXTURE_BIN="$TARGET_DIR/debug/tidy-fixture-home"

# ---------------------------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------------------------

TMP_ROOT="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
FX_BASE="$(mktemp -d "$TMP_ROOT/tidy-accept.XXXXXX")"
echo "$FX_BASE" >"$RUN/fixture-base.txt"

# Deletes the fixture base folder this run created, and nothing else.
remove_fixture_base() {
  case "$FX_BASE" in
    "$TMP_ROOT"/tidy-accept.?*) ;;
    *) return 0 ;;
  esac
  case "$FX_BASE" in
    *..* | *"*"* | *"?"*) return 0 ;;
  esac
  if [ -d "$FX_BASE" ]; then rm -rf -- "$FX_BASE"; fi
}
ACCEPT_EXTRA_CLEANUP=remove_fixture_base

FX_DIR="" FX_HOME="" FX_TIDY="" FX_NOW="" FX_RUNNING=""

# make_fixture <name>: builds the standard fake home in $FX_BASE/<name> and sets FX_*.
make_fixture() {
  local name=$1 line
  FX_DIR="$FX_BASE/$name"
  if ! line=$("$FIXTURE_BIN" "$FX_DIR" 2>"$RUN/$name.fixture.err"); then
    note "tidy-fixture-home failed: $(cat "$RUN/$name.fixture.err")"
    return 1
  fi
  printf '%s\n' "$line" >"$RUN/$name.fixture.json"
  FX_HOME=$(printf '%s' "$line" | jq -r .home)
  FX_TIDY=$(printf '%s' "$line" | jq -r .tidy_home)
  FX_NOW=$(printf '%s' "$line" | jq -r .test_now)
  FX_RUNNING=$(printf '%s' "$line" | jq -r .test_running)
  [ -d "$FX_HOME" ] && [ -d "$FX_TIDY" ] && [ -n "$FX_NOW" ] && [ -n "$FX_RUNNING" ]
}

# fx_run <command...>: runs a command against the current fixture. TIDY_HOME is always set.
fx_run() {
  env TIDY_HOME="$FX_TIDY" TIDY_SCAN_ROOT="$FX_HOME" TIDY_TEST_NOW="$FX_NOW" \
    TIDY_TEST_RUNNING="$FX_RUNNING" "$@"
}

# listing <dir>: every path below dir, sorted bytewise, one per line (symlinks not followed).
listing() { (cd "$1" && find . | LC_ALL=C sort); }

# The jq filters that pick the compared fields, as in crates/tidy-cli/tests/cli.rs.
PLAN_FILTER='[.items[] | {id, group, class, ticked, blocked, bytes_alloc}] | sort_by(.id)'
LARGE_FILTER='[.items[] | {rel, name, kind, bytes_alloc, bytes_logical, app_data}]'

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

# ---------------------------------------------------------------------------------------------
# AC1: gates
# ---------------------------------------------------------------------------------------------

ac1() {
  clear_tidy_env
  if [ -n "${ACCEPT_SKIP_GATES:-}" ]; then
    skip AC1 "ACCEPT_SKIP_GATES is set: the gates were not run"
    if ! run_logged build-tools cargo build -p tidy-cli -p tidy-fixtures; then
      note "cannot build tidyctl and tidy-fixture-home, see $LAST_LOG"
    fi
    return
  fi
  if run_logged build cargo build --workspace --all-targets &&
    run_logged fmt cargo fmt --all --check &&
    run_logged clippy cargo clippy --workspace --all-targets -- -D warnings &&
    run_logged test-core cargo test --workspace --exclude tidy-app &&
    run_logged test-app cargo test -p tidy-app -- --test-threads=1; then
    pass AC1 "build, fmt, clippy, tests"
  else
    fail AC1 "a gate failed, see $LAST_LOG"
  fi
}

# ---------------------------------------------------------------------------------------------
# AC2: the cleanup scan is exact
# ---------------------------------------------------------------------------------------------

ac2() {
  clear_tidy_env
  local out="$RUN/scan.out" bad="" rc n
  if [ ! -x "$TIDYCTL" ] || [ ! -x "$FIXTURE_BIN" ]; then
    fail AC2 "missing $TIDYCTL or $FIXTURE_BIN"
    return
  fi
  if ! make_fixture fx-scan; then
    fail AC2 "could not build the fixture"
    return
  fi
  fx_run "$TIDYCTL" scan >"$out" 2>"$RUN/scan.err"
  rc=$?
  [ $rc -eq 0 ] || bad="$bad [exit $rc]"

  # Every line is a JSON object with an event key; the last one is the plan.
  jq -s -e 'length > 0 and all(.[]; type == "object" and has("event"))' "$out" >/dev/null 2>&1 ||
    bad="$bad [a line is not a JSON event]"
  tail -n 1 "$out" | jq -e '.event == "plan"' >/dev/null 2>&1 || bad="$bad [last line is not the plan]"

  tail -n 1 "$out" | jq -S -c "$PLAN_FILTER" >"$RUN/scan.actual.json" 2>/dev/null
  jq -S -c "$PLAN_FILTER" "$FX_DIR/expected-plan.json" >"$RUN/scan.expected.json"
  cmp -s "$RUN/scan.actual.json" "$RUN/scan.expected.json" ||
    bad="$bad [plan differs: diff $RUN/scan.actual.json $RUN/scan.expected.json]"
  n=$(jq 'length' "$RUN/scan.expected.json")

  # No excluded or denylisted path anywhere in the output.
  local never
  while IFS= read -r never; do
    [ -n "$never" ] || continue
    if grep -qF -- "$never" "$out"; then
      bad="$bad [output mentions $never]"
    fi
  done < <(jq -r '.never[]' "$FX_DIR/expected-plan.json")

  if [ -z "$bad" ]; then
    pass AC2 "scan matches expected-plan.json ($n items), no excluded path in the output"
  else
    fail AC2 "$bad"
  fi
}

# ---------------------------------------------------------------------------------------------
# AC3: clean moves exactly the safe set
# ---------------------------------------------------------------------------------------------

ac3() {
  clear_tidy_env
  local bad="" rc
  if [ ! -x "$TIDYCTL" ] || [ ! -x "$FIXTURE_BIN" ]; then
    fail AC3 "missing $TIDYCTL or $FIXTURE_BIN"
    return
  fi
  if ! make_fixture fx-clean; then
    fail AC3 "could not build the fixture"
    return
  fi
  local plan="$FX_DIR/expected-plan.json"
  local before="$RUN/clean.before.txt" after="$RUN/clean.after.txt" want="$RUN/clean.want-after.txt"
  local rels="$RUN/clean.safe-rels.txt" hist="$FX_TIDY/data/history.jsonl" trash="$FX_TIDY/data/trash"
  listing "$FX_HOME" >"$before"

  # The ticked Safe items of the expected plan, as paths relative to the scan root.
  jq -r '.items[] | select(.ticked and .class == "safe") | .id | sub("^[^:]*:"; "")' "$plan" >"$rels"
  local nsafe
  nsafe=$(wc -l <"$rels" | tr -d ' ')
  [ "$nsafe" -gt 0 ] || bad="$bad [the expected plan has no ticked safe item]"

  fx_run "$TIDYCTL" clean --safe >"$RUN/clean.out" 2>"$RUN/clean.err"
  rc=$?
  [ $rc -eq 0 ] || bad="$bad [clean exit $rc]"
  listing "$FX_HOME" >"$after"

  # The listing after equals the listing before minus the safe items and what is under them.
  awk -v rels="$rels" '
    BEGIN { while ((getline l < rels) > 0) r[l] = 1 }
    {
      p = substr($0, 3); drop = 0; acc = ""
      n = split(p, a, "/")
      for (i = 1; i <= n; i++) {
        acc = (i == 1) ? a[1] : acc "/" a[i]
        if (acc in r) { drop = 1; break }
      }
      if (!drop) print
    }' "$before" >"$want"
  cmp -s "$want" "$after" || bad="$bad [home listing differs from the expected one: diff $want $after]"
  local gone
  gone=$(($(wc -l <"$before") - $(wc -l <"$after")))
  [ "$gone" -gt 0 ] || bad="$bad [nothing left the home folder]"

  # History: one clean entry, DirTrash, one item per safe item with its bytes.
  if [ -s "$hist" ]; then
    jq -s -e '[.[] | select(.action == "clean")] | length == 1' "$hist" >/dev/null 2>&1 ||
      bad="$bad [history does not hold exactly one clean entry]"
    jq -s -e '[.[] | select(.action == "clean")][0].trasher == "dir"' "$hist" >/dev/null 2>&1 ||
      bad="$bad [trasher is not dir]"
    jq -s -e '[.[] | select(.action == "clean")][0] | .items | length > 0 and all(.[]; .result == "trashed" and (.bytes | type == "number"))' \
      "$hist" >/dev/null 2>&1 || bad="$bad [a history item is not trashed or has no bytes]"
    jq -s -S -c '[.[] | select(.action == "clean")][0] | [.items[] | {path, bytes}] | sort_by(.path)' "$hist" \
      >"$RUN/clean.hist-items.json" 2>/dev/null
    jq -S -c --arg home "$FX_HOME" \
      '[.items[] | select(.ticked and .class == "safe") | {path: ($home + "/" + (.id | sub("^[^:]*:"; ""))), bytes: .bytes_alloc}] | sort_by(.path)' \
      "$plan" >"$RUN/clean.want-items.json"
    cmp -s "$RUN/clean.hist-items.json" "$RUN/clean.want-items.json" ||
      bad="$bad [history items differ from the safe set: diff $RUN/clean.hist-items.json $RUN/clean.want-items.json]"
  else
    bad="$bad [no history.jsonl]"
  fi

  # The Trash folder holds one top-level entry per safe item, and each recorded trash path exists.
  local ntrash missing=0 tp
  ntrash=$(find "$trash" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l | tr -d ' ')
  [ "$ntrash" -eq "$nsafe" ] || bad="$bad [trash holds $ntrash entries, expected $nsafe]"
  while IFS= read -r tp; do
    case "$tp" in
      "$trash"/?*) [ -e "$tp" ] || missing=$((missing + 1)) ;;
      *) missing=$((missing + 1)) ;;
    esac
  done < <(jq -r '.items[].trash_path' "$hist" 2>/dev/null)
  [ "$missing" -eq 0 ] || bad="$bad [$missing recorded trash paths missing or outside the trash]"

  # Without TIDY_HOME the command refuses with exit 5 and moves nothing. HOME points at an empty
  # temporary folder, so even a regression could not reach the real home folder.
  local nohome="$FX_BASE/no-home" rc5
  mkdir -p "$nohome"
  env -i PATH="$PATH" HOME="$nohome" "$TIDYCTL" clean --safe >"$RUN/clean-refused.out" 2>"$RUN/clean-refused.err"
  rc5=$?
  [ $rc5 -eq 5 ] || bad="$bad [clean without TIDY_HOME exited $rc5, expected 5]"
  listing "$FX_HOME" >"$RUN/clean.after-refused.txt"
  cmp -s "$after" "$RUN/clean.after-refused.txt" || bad="$bad [the refused clean changed the fixture home]"
  [ -z "$(ls -A "$nohome")" ] || bad="$bad [the refused clean wrote into HOME]"

  if [ -z "$bad" ]; then
    pass AC3 "moved exactly the $nsafe safe items to the dir trash; clean without TIDY_HOME exits 5"
  else
    fail AC3 "$bad"
  fi
}

# ---------------------------------------------------------------------------------------------
# AC4: Large files is exact
# ---------------------------------------------------------------------------------------------

ac4() {
  clear_tidy_env
  local bad="" rc
  if [ ! -x "$TIDYCTL" ] || [ ! -x "$FIXTURE_BIN" ]; then
    fail AC4 "missing $TIDYCTL or $FIXTURE_BIN"
    return
  fi
  if ! make_fixture fx-large; then
    fail AC4 "could not build the fixture"
    return
  fi
  local exp="$FX_DIR/expected-large.json"

  # Items over 4 MB, largest first.
  fx_run "$TIDYCTL" large --min-mb 4 >"$RUN/large.out" 2>"$RUN/large.err"
  rc=$?
  [ $rc -eq 0 ] || bad="$bad [large exit $rc]"
  tail -n 1 "$RUN/large.out" | jq -e '.event == "large"' >/dev/null 2>&1 || bad="$bad [last line is not the large result]"
  tail -n 1 "$RUN/large.out" | jq -S -c "$LARGE_FILTER" >"$RUN/large.actual.json" 2>/dev/null
  jq -S -c "$LARGE_FILTER" "$exp" >"$RUN/large.expected.json"
  cmp -s "$RUN/large.actual.json" "$RUN/large.expected.json" ||
    bad="$bad [items differ: diff $RUN/large.actual.json $RUN/large.expected.json]"
  # Big.app is one item, and the absent paths are not listed or contained in a listed item.
  tail -n 1 "$RUN/large.out" | jq -e '[.items[] | select(.rel == "Applications/Big.app")] | length == 1' >/dev/null 2>&1 ||
    bad="$bad [Big.app is not one item]"
  local a
  while IFS= read -r a; do
    [ -n "$a" ] || continue
    if tail -n 1 "$RUN/large.out" | jq -e --arg a "$a" 'any(.items[]; .rel == $a)' >/dev/null 2>&1; then
      bad="$bad [$a is listed]"
    fi
  done < <(jq -r '.absent_items[]' "$exp")

  # Not opened: only the item dated 200 days back.
  fx_run "$TIDYCTL" large --min-mb 4 --not-opened >"$RUN/large-not-opened.out" 2>"$RUN/large-not-opened.err"
  rc=$?
  [ $rc -eq 0 ] || bad="$bad [large --not-opened exit $rc]"
  tail -n 1 "$RUN/large-not-opened.out" | jq -S -c '[.items[].rel]' >"$RUN/large.no.actual.json" 2>/dev/null
  jq -S -c '.not_opened' "$exp" >"$RUN/large.no.expected.json"
  cmp -s "$RUN/large.no.actual.json" "$RUN/large.no.expected.json" ||
    bad="$bad [not-opened list differs: diff $RUN/large.no.actual.json $RUN/large.no.expected.json]"
  tail -n 1 "$RUN/large-not-opened.out" |
    jq -e '(.items | length) == 1 and (((.now - .items[0].last_used) / 86400 | floor) == 200)' >/dev/null 2>&1 ||
    bad="$bad [the not-opened item is not dated 200 days back]"

  # Folder totals.
  fx_run "$TIDYCTL" large --min-mb 4 --folders >"$RUN/large-folders.out" 2>"$RUN/large-folders.err"
  rc=$?
  [ $rc -eq 0 ] || bad="$bad [large --folders exit $rc]"
  tail -n 1 "$RUN/large-folders.out" | jq -e '.event == "folders"' >/dev/null 2>&1 || bad="$bad [last line is not folders]"
  local nf
  nf=$(jq '.folders | length' "$exp")
  if ! jq -n -e --slurpfile exp "$exp" --slurpfile act <(tail -n 1 "$RUN/large-folders.out") \
    'all($exp[0].folders[]; . as $e | any($act[0].folders[]; .rel == $e.rel and .bytes_alloc == $e.bytes_alloc and .bytes_logical == $e.bytes_logical))' \
    >/dev/null 2>&1; then
    bad="$bad [a folder total differs from expected-large.json]"
  fi

  # Nothing from the denylisted folders or behind the symlink, in any of the outputs.
  local never
  while IFS= read -r never; do
    [ -n "$never" ] || continue
    if grep -qF -- "$never" "$RUN/large.out" "$RUN/large-not-opened.out" "$RUN/large-folders.out"; then
      bad="$bad [output mentions $never]"
    fi
  done < <(jq -r '.never[]' "$exp")

  if [ -z "$bad" ]; then
    pass AC4 "large, --not-opened and --folders ($nf totals) match expected-large.json"
  else
    fail AC4 "$bad"
  fi
}

# ---------------------------------------------------------------------------------------------
# AC5: bundle and background screenshots
# ---------------------------------------------------------------------------------------------

ac5() {
  clear_tidy_env
  if [ -n "${ACCEPT_SKIP_BUNDLE:-}" ]; then
    skip AC5 "ACCEPT_SKIP_BUNDLE is set: the signed bundle and the screenshots were not made"
    return
  fi
  local app="$TARGET_DIR/bundle/Tidy.app"
  local plist="$app/Contents/Info.plist"
  local pb=/usr/libexec/PlistBuddy
  local detail="" bad="" v

  if ! run_logged bundle "$REPO/scripts/bundle.sh" --release; then
    fail AC5 "bundle.sh --release failed, see $LAST_LOG"
    return
  fi
  if run_logged codesign codesign --verify --strict --verbose=2 "$app"; then
    detail="$detail codesign-verify"
  else
    bad="$bad [codesign verify failed]"
  fi
  if run_logged plutil plutil -lint "$plist"; then
    detail="$detail plutil"
  else
    bad="$bad [plutil failed]"
  fi

  # The designated requirement names the bundle id and the Apple Development certificate.
  local dr
  dr=$(codesign -d -r- "$app" 2>&1)
  if printf '%s' "$dr" | grep -q 'identifier "dev.sdusteric.tidy"' &&
    printf '%s' "$dr" | grep -q 'Apple Development'; then
    detail="$detail designated-requirement"
  else
    bad="$bad [designated requirement lacks dev.sdusteric.tidy or Apple Development: $dr]"
  fi

  # Info.plist content: executable, id, minimum system, the three folder strings, and none of
  # the URL scheme or document type keys.
  v=$("$pb" -c 'Print :CFBundleExecutable' "$plist" 2>/dev/null)
  [ "$v" = "tidy" ] || bad="$bad [CFBundleExecutable=$v]"
  v=$("$pb" -c 'Print :CFBundleIdentifier' "$plist" 2>/dev/null)
  [ "$v" = "dev.sdusteric.tidy" ] || bad="$bad [CFBundleIdentifier=$v]"
  v=$("$pb" -c 'Print :LSMinimumSystemVersion' "$plist" 2>/dev/null)
  [ "$v" = "14.0" ] || bad="$bad [LSMinimumSystemVersion=$v]"
  for v in NSSupportsAutomaticTermination NSSupportsSuddenTermination; do
    [ "$("$pb" -c "Print :$v" "$plist" 2>/dev/null)" = "false" ] || bad="$bad [$v is not false]"
  done
  for v in NSDocumentsFolderUsageDescription NSDesktopFolderUsageDescription NSDownloadsFolderUsageDescription; do
    [ -n "$("$pb" -c "Print :$v" "$plist" 2>/dev/null)" ] || bad="$bad [no $v]"
  done
  for v in CFBundleURLTypes CFBundleDocumentTypes NSLocalNetworkUsageDescription; do
    if "$pb" -c "Print :$v" "$plist" >/dev/null 2>&1; then
      bad="$bad [$v is present]"
    fi
  done
  local macos
  macos=$(ls "$app/Contents/MacOS")
  [ "$macos" = "tidy" ] || bad="$bad [Contents/MacOS holds: $macos]"

  # --self-test: JSON report, exit 0, no window. A fresh TIDY_HOME keeps it off the real folders.
  local home="$RUN/home-selftest"
  remove_run_path "$home"
  mkdir -p "$home"
  if env TIDY_HOME="$home" TIDY_BACKGROUND=1 "$app/Contents/MacOS/tidy" --self-test >"$RUN/selftest.json" 2>"$RUN/selftest.err" &&
    jq -e '.ok == true' "$RUN/selftest.json" >/dev/null 2>&1; then
    detail="$detail self-test"
  else
    bad="$bad [--self-test failed, see $RUN/selftest.json]"
  fi

  # Screenshots: the offscreen render bundle, then four PNGs at 1800 x 1240 px.
  if ! run_logged shots-bundle "$REPO/scripts/shots-bundle.sh"; then
    fail AC5 "shots-bundle.sh failed, see $LAST_LOG$bad"
    return
  fi
  local stamp="$RUN/shots.stamp" out="${OUT:-$REPO/docs/app-shots}" before after
  : >"$stamp"
  sleep 1
  before=$(front)
  if run_logged shots env SHOTS_RENDER=1 "$REPO/scripts/shots.sh" cleanup large; then
    detail="$detail shots"
  else
    bad="$bad [shots.sh failed, see $LAST_LOG]"
  fi
  after=$(front)
  [ "$before" = "$after" ] || bad="$bad [frontmost app changed: $before -> $after]"
  local f w h
  for f in cleanup-light cleanup-dark large-light large-dark; do
    if [ ! -s "$out/$f.png" ] || [ ! "$out/$f.png" -nt "$stamp" ]; then
      bad="$bad [$f.png was not written by this run]"
      continue
    fi
    w=$(sips -g pixelWidth "$out/$f.png" | awk '/pixelWidth/{print $2}')
    h=$(sips -g pixelHeight "$out/$f.png" | awk '/pixelHeight/{print $2}')
    [ "${w}x${h}" = "1800x1240" ] || bad="$bad [$f.png is ${w}x${h}]"
  done

  if [ -z "$bad" ]; then
    pass AC5 "signed bundle:$detail; 4 shots at 1800x1240; frontmost app unchanged"
  else
    fail AC5 "bundle and shots:$detail$bad"
  fi
}

# ---------------------------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------------------------

ac1
ac2
ac3
ac4
ac5

echo
echo "== summary"
npass=$(awk -F'\t' '$2 == "PASS"' "$RESULTS" | wc -l | tr -d ' ')
nfail=$(awk -F'\t' '$2 == "FAIL"' "$RESULTS" | wc -l | tr -d ' ')
nskip=$(awk -F'\t' '$2 == "SKIP"' "$RESULTS" | wc -l | tr -d ' ')
echo "PASS $npass, FAIL $nfail, SKIP $nskip. Results: $RESULTS"
if [ "$nfail" -gt 0 ]; then
  awk -F'\t' '$2 == "FAIL" { printf "  FAIL %s %s\n", $1, $3 }' "$RESULTS"
  exit 1
fi
exit 0
