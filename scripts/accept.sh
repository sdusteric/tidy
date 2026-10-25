#!/usr/bin/env bash
# Tidy acceptance runner, milestones 1 and 2.
#
#   scripts/accept.sh offline
#
# Prints PASS, FAIL or SKIP for each criterion id (AC1 to AC9) and exits nonzero on any FAIL.
# The criteria are in docs/superpowers/specs/2026-10-23-tidy-design.md, sections 13 and 16.
#
#   AC1  gates: build, fmt, clippy, tests
#   AC2  tidyctl scan on the fixture home equals expected-plan.json
#   AC3  tidyctl clean --safe moves exactly the safe set; without TIDY_HOME it exits 5
#   AC4  tidyctl large equals expected-large.json (items, --not-opened, --folders)
#   AC5  signed bundle, Info.plist, --self-test, offscreen screenshots at 1800 x 1240 px
#   AC6  Memory: grouping and Quit tests, tidyctl memory on this Mac against sysctl, vm_stat, top
#   AC7  Security: parser, signer and diff tests, tidyctl security on this Mac (read-only)
#   AC8  Check up: tidyctl checkup on the fixture home, reminder math and permission tests
#   AC9  App: sidebar order and Cmd-1 to Cmd-5, long paths, shots of all five screens
#   U1   update contract on the bundle of AC5: Sparkle embedded and signed, the contract key,
#        only the app links Sparkle (scripts/check-updates.sh)
#
# Environment:
#   ACCEPT_SKIP_BUNDLE=1  skip AC5, U1 and the shots of AC9 (they sign with the keychain and
#                         run release builds). They then show as SKIP.
#   ACCEPT_SKIP_GATES=1   skip AC1 (the full build and test run), for quick runs while
#                         developing. AC1 then shows as SKIP. tidyctl and tidy-fixture-home are
#                         still built, because the other criteria need them.
#
# A full run has FAIL 0 and SKIP 0.
#
# Safety: every tidyctl run that scans or cleans sets TIDY_HOME and TIDY_SCAN_ROOT to a fixture
# inside a fresh folder under $TMPDIR, so the real home folder, the real Trash and the real Tidy
# folders are never read or written. (The fixture cannot live under target/accept: the repo is
# under ~/Documents, which tidy-fixture-home refuses.) AC6 and AC7 also read this Mac, read-only:
# the process table, sysctl and vm_stat, `top`, fdesetup, socketfilterfw, spctl, csrutil, the
# XProtect plist, the launch agent and daemon folders and the signatures of the programs they
# name. They run with TIDY_HOME in a temp folder, never run softwareupdate, never quit or signal
# any app, and never ask for a permission. The app is never started except for
# `tidy --self-test` and the offscreen screenshots of scripts/shots.sh with SHOTS_RENDER=1,
# both with TIDY_BACKGROUND=1 and a temporary TIDY_HOME. Nothing is installed. Every process
# this script starts is stopped by PID when it exits. Logs are in target/accept/<timestamp>.

# shellcheck source=scripts/accept-lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/accept-lib.sh"

usage() {
  sed -n '2,34p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
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
    skip U1 "ACCEPT_SKIP_BUNDLE is set"
    return
  fi
  local app="$TARGET_DIR/bundle/Tidy.app"
  local plist="$app/Contents/Info.plist"
  local pb=/usr/libexec/PlistBuddy
  local detail="" bad="" v

  if ! run_logged bundle "$REPO/scripts/bundle.sh" --release; then
    fail AC5 "bundle.sh --release failed, see $LAST_LOG"
    fail U1 "no bundle to check"
    return
  fi
  # U1: Sparkle is embedded and signed, the key and feed are the contract's, and only the app
  # links it (tidyctl from the AC1 build does not).
  if run_logged check-updates "$REPO/scripts/check-updates.sh" "$app" "$TIDYCTL"; then
    pass U1 "update contract: framework, SUPublicEDKey, app links Sparkle, tidyctl does not, codesign --deep"
  else
    fail U1 "update contract: $(grep '^FAIL' "$LAST_LOG" | tr '\n' ' ')"
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

  # The designated requirement names the bundle id and the signing certificate (Apple Development, or Developer ID once it exists).
  local dr
  dr=$(codesign -d -r- "$app" 2>&1)
  if printf '%s' "$dr" | grep -q 'identifier "dev.sdusteric.tidy"' &&
    printf '%s' "$dr" | grep -Eq 'Apple Development|Developer ID Application|1\.2\.840\.113635\.100\.6\.1\.13'; then
    detail="$detail designated-requirement"
  else
    bad="$bad [designated requirement lacks dev.sdusteric.tidy or an Apple Development or Developer ID certificate: $dr]"
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
# Helpers for the milestone 2 criteria
# ---------------------------------------------------------------------------------------------

# now_ms: milliseconds since the epoch (BSD date has no %N).
now_ms() { perl -MTime::HiRes=time -e 'printf "%d\n", time() * 1000'; }

# run_named <log name> "<cargo test args>" <test name>...: runs `cargo test <args>` filtered to
# the named tests, one thread, and fails unless every name ran and passed.
run_named() {
  local name=$1 args=$2 n missing=""
  shift 2
  # shellcheck disable=SC2086
  if ! run_logged "$name" cargo test $args -- --test-threads=1 "$@"; then
    return 1
  fi
  for n in "$@"; do
    grep -Eq "^test .*${n} \.\.\. ok\$" "$LAST_LOG" || missing="$missing $n"
  done
  if [ -n "$missing" ]; then
    note "tests that did not run or did not pass:$missing"
    return 1
  fi
}

# The system figures that tidyctl memory must agree with, taken with the tools macOS ships.
ref_pressure_level() { sysctl -n kern.memorystatus_vm_pressure_level; }

# Swap used, in bytes, from "vm.swapusage: total = 21504.00M  used = 19944.81M  free = ...".
ref_swap_used() {
  sysctl -n vm.swapusage | awk '{
    for (i = 1; i <= NF; i++) if ($i == "used") {
      v = $(i + 2); u = substr(v, length(v)); n = substr(v, 1, length(v) - 1)
      m = 1
      if (u == "K") m = 1024; else if (u == "M") m = 1048576; else if (u == "G") m = 1073741824
      printf "%.0f\n", n * m
    }
  }'
}

# Compressed memory, in bytes: "Pages occupied by compressor" times the page size of vm_stat.
ref_compressed() {
  vm_stat | awk '
    /page size of/ { for (i = 1; i <= NF; i++) if ($i == "of") ps = $(i + 1) }
    /Pages occupied by compressor/ { n = $NF; gsub(/\./, "", n) }
    END { printf "%.0f\n", n * ps }'
}

# The pids `top` lists as the biggest by memory, one per line.
# The five processes top lists by memory, one "pid bytes" line each (top's MEM column, in
# bytes; 0 when it has no unit top uses).
ref_top_pids() {
  top -l 1 -o mem -n 5 -stats pid,mem,command 2>/dev/null |
    awk '/^PID/ { f = 1; next }
      f && $1 ~ /^[0-9]+$/ {
        m = $2; sub(/[+-]$/, "", m); u = substr(m, length(m)); n = substr(m, 1, length(m) - 1) + 0
        k = (u == "B") ? 1 : (u == "K") ? 1024 : (u == "M") ? 1048576 : (u == "G") ? 1073741824 : (u == "T") ? 1099511627776 : 0
        printf "%s %.0f\n", $1, n * k
      }'
}

# ---------------------------------------------------------------------------------------------
# AC6: Memory
# ---------------------------------------------------------------------------------------------

ac6() {
  clear_tidy_env
  local bad="" detail="" out="$RUN/memory.json" mem_home="$FX_BASE/mem-home" attempt
  if [ ! -x "$TIDYCTL" ]; then
    fail AC6 "missing $TIDYCTL"
    return
  fi

  # Core tests on a fake process table (the MemoryProbe seam).
  if run_named memory-core "-p tidy-core --test memory" \
    helpers_are_grouped_under_the_responsible_app \
    the_parent_chain_is_the_fallback_without_a_responsible_pid \
    the_app_path_is_the_last_fallback \
    unreadable_processes_are_summed_into_one_system_row \
    the_system_row_names_windowserver_first \
    the_system_row_sums_the_sizes_top_gave \
    top_samples_parse \
    tidy_finder_windowserver_loginwindow_and_kernel_task_have_no_quit \
    snapshot_carries_the_figures_the_verdict_and_the_rows; then
    detail="$detail core-tests"
  else
    bad="$bad [memory core tests failed, see $LAST_LOG]"
  fi

  # App tests with a fake terminator and a fake clock.
  if run_named memory-app "-p tidy-app" \
    memory_polls_every_2_seconds_only_while_it_is_visible \
    quit_sends_a_polite_terminate_then_says_quit_requested \
    after_10_seconds_the_row_says_still_open_and_force_quit_needs_a_confirm \
    tidy_finder_and_system_can_never_be_quit \
    the_test_harness_has_only_fakes_for_quitting_and_notifying; then
    detail="$detail app-tests"
  else
    bad="$bad [memory app tests failed, see $LAST_LOG]"
  fi

  # No test signals a process it did not spawn: the app tests use only the fake terminator, and
  # the only kill in the core tests is of a child the test started itself.
  local sig
  sig=$(grep -rnE 'SystemTerminator|SystemNotifier|libc::kill|killpg|pkill|killall|forceTerminate' \
    "$REPO"/crates/*/tests "$REPO/crates/tidy-app/src/tests.rs" 2>/dev/null || true)
  if [ -n "$sig" ]; then
    bad="$bad [a test names a real terminator, notifier or signal: $sig]"
  else
    detail="$detail no-real-signals"
  fi

  # tidyctl memory on this Mac: exit 0 in under 3 s, and the figures agree with sysctl and
  # vm_stat taken right after (within 10%). The pressure level is exact. The values drift while
  # the commands run, so a mismatch is tried again up to three times.
  mkdir -p "$mem_home"
  local rc ms t0 t1 level swap comp tops pid row_bad sys_named
  for attempt in 1 2 3; do
    row_bad=""
    t0=$(now_ms)
    env TIDY_HOME="$mem_home" "$TIDYCTL" memory >"$out" 2>"$RUN/memory.err"
    rc=$?
    t1=$(now_ms)
    ms=$((t1 - t0))
    level=$(ref_pressure_level)
    swap=$(ref_swap_used)
    comp=$(ref_compressed)
    tops=$(ref_top_pids)
    [ $rc -eq 0 ] || row_bad="$row_bad [exit $rc]"
    [ "$ms" -lt 3000 ] || row_bad="$row_bad [took ${ms} ms]"
    if ! tail -n 1 "$out" | jq -e --argjson lvl "${level:-0}" --argjson swap "${swap:-0}" --argjson comp "${comp:-0}" '
        def near($a; $b): (($a - $b) | fabs) <= (0.1 * ([$a, $b] | max));
        .event == "memory"
        and .pressure_level == $lvl
        and .pressure == ({"1": "normal", "2": "warning", "4": "critical"}[($lvl | tostring)])
        and near(.swap.used; $swap)
        and near(.compressed_bytes; $comp)' >/dev/null 2>&1; then
      row_bad="$row_bad [figures differ from sysctl/vm_stat: level $level swap $swap compressed $comp; got $(tail -n 1 "$out" | jq -c '{pressure_level, swap: .swap.used, compressed_bytes}' 2>/dev/null)]"
    fi
    [ -n "$tops" ] || row_bad="$row_bad [top listed no process]"
    # Each process top shows must be in a named row with its size. A process of another user
    # (WindowServer runs as _windowserver) cannot be read by an app (EPERM from
    # proc_pid_rusage, PROC_PIDTASKINFO and memorystatus_control), so Tidy sizes it with
    # top, which is setuid root. Such a process may be in the System row only if the row
    # names it and the row's size holds at least 90% of what top shows for it.
    local me kind pname puid tmem sysfp
    me=$(id -u)
    sys_named=""
    while read -r pid tmem; do
      [ -n "$pid" ] || continue
      kind=$(tail -n 1 "$out" | jq -r --argjson p "$pid" 'first(.apps[] | select((.pids // []) | index($p) != null) | .kind) // "none"' 2>/dev/null)
      case "$kind" in
        app | process) ;;
        system)
          puid=$(ps -o uid= -p "$pid" 2>/dev/null | tr -d ' ')
          pname=$(ps -o comm= -p "$pid" 2>/dev/null)
          pname=${pname##*/}
          if [ -z "$puid" ]; then
            : # it exited since top ran; the retry loop does not need it
          elif [ "$puid" = "$me" ]; then
            row_bad="$row_bad [top pid $pid ($pname) is this user's but has no size]"
          elif ! tail -n 1 "$out" | jq -e --arg n "$pname" 'any(.apps[] | select(.kind == "system"); any((.members // [])[]; . as $m | ($m | length) > 0 and ($m == $n or ($m | startswith($n)) or ($n | startswith($m)))))' >/dev/null 2>&1; then
            row_bad="$row_bad [top pid $pid ($pname, uid $puid) is in the System row, which does not name it]"
          else
            sysfp=$(tail -n 1 "$out" | jq -r 'first(.apps[] | select(.kind == "system") | .footprint) // 0' 2>/dev/null)
            if [ "${tmem:-0}" -le 0 ] || [ "${sysfp:-0}" -lt $((tmem * 9 / 10)) ]; then
              row_bad="$row_bad [top pid $pid ($pname) shows $tmem bytes but the System row has ${sysfp:-0}]"
            else
              sys_named="$sys_named $pname"
            fi
          fi
          ;;
        *) row_bad="$row_bad [top pid $pid is in no row]" ;;
      esac
    done <<<"$tops"
    [ -z "$row_bad" ] && break
  done
  if [ -z "$row_bad" ]; then
    detail="$detail tidyctl-memory(${ms}ms,try$attempt)"
    [ -z "$sys_named" ] || detail="$detail in-system-row-sized-by-top:$(echo $sys_named | tr ' ' ',')"
  else
    bad="$bad$row_bad"
  fi
  # The snapshot rows: every row has a name and a footprint, and Finder and Tidy are not quittable.
  tail -n 1 "$out" | jq -e '[.apps[] | select(.kind == "app" or .kind == "process" or .kind == "system")] | length > 0 and all(.[]; (.name | type == "string") and (.footprint | type == "number"))' >/dev/null 2>&1 ||
    bad="$bad [a memory row has no name or footprint]"
  tail -n 1 "$out" | jq -e 'all(.apps[] | select(.kind == "system"); .quittable == false) and all(.apps[] | select(.name == "Finder" or .name == "Tidy"); .quittable == false)' >/dev/null 2>&1 ||
    bad="$bad [the System row, Finder or Tidy can be quit]"
  [ ! -e "$mem_home/data/checkups.jsonl" ] || bad="$bad [memory wrote a check up]"

  if [ -z "$bad" ]; then
    pass AC6 "memory:$detail"
  else
    fail AC6 "memory:$detail$bad"
  fi
}

# ---------------------------------------------------------------------------------------------
# AC7: Security
# ---------------------------------------------------------------------------------------------

ac7() {
  clear_tidy_env
  local bad="" detail="" out="$RUN/security.out" sec_home="$FX_BASE/sec-home"
  if [ ! -x "$TIDYCTL" ]; then
    fail AC7 "missing $TIDYCTL"
    return
  fi

  # Core tests: captured real outputs (on and off variants), XProtect age with a FixedClock,
  # launch agent and daemon folders, the Apple and unsigned signers, Chrome Secure Preferences,
  # the diff against the last check, and softwareupdate never running.
  if run_named security-core "-p tidy-core --test security" \
    captured_outputs_from_this_mac_parse \
    off_and_disabled_variants_are_amber_with_their_button \
    gatekeeper_off_answers_with_exit_1_and_is_amber \
    failures_timeouts_and_unknown_output_are_gray_never_green \
    xprotect_is_amber_after_60_days \
    launchd_fixture_folders_parse_xml_and_binary_plists \
    apple_and_unsigned_programs_are_told_apart \
    login_items_carry_label_program_run_at_load_and_signer \
    chrome_secure_preferences_list_the_extensions_people_installed \
    each_check_marks_new_and_changed_against_the_last_and_the_first_run_says_so \
    only_the_newest_runs_are_kept \
    a_run_without_include_updates_never_runs_softwareupdate \
    security_sources_use_no_scare_words; then
    detail="$detail core-tests"
  else
    bad="$bad [security core tests failed, see $LAST_LOG]"
  fi
  if run_named security-app "-p tidy-app" \
    rows_have_dots_marks_and_amber_buttons_but_gray_rows_have_none \
    the_first_run_says_so \
    amber_buttons_open_the_right_settings_page_only_from_a_click; then
    detail="$detail app-tests"
  else
    bad="$bad [security app tests failed, see $LAST_LOG]"
  fi

  # No scare words in any user-facing string. A hit in a comment or in an identifier such as
  # the danger_button color token (design section 11) is not a string, so only the text between
  # double quotes is searched.
  local scare
  scare=$(grep -rnE '"([^"\\]|\\.)*"' "$REPO"/crates/*/src --include='*.rs' |
    perl -ne 'while (/"((?:[^"\\]|\\.)*)"/g) { if ($1 =~ /threat|infected|danger|risk score/i) { print $_; last } }')
  if [ -n "$scare" ]; then
    bad="$bad [a string has a scare word: $scare]"
  else
    detail="$detail no-scare-words"
  fi

  # tidyctl security on this Mac: exit 0 in under 20 s, every check of 5.4 listed with a status,
  # macOS updates not run (gray, on demand), softwareupdate never started, no dialog.
  mkdir -p "$sec_home"
  local before after t0 t1 ms rc pid sw=0
  before=$(front)
  t0=$(now_ms)
  env TIDY_HOME="$sec_home" "$TIDYCTL" security >"$out" 2>"$RUN/security.err" &
  pid=$!
  track_pid "$pid"
  while pid_alive "$pid"; do
    if pgrep -x softwareupdate >/dev/null 2>&1; then sw=1; fi
    sleep 0.05
  done
  wait "$pid"
  rc=$?
  t1=$(now_ms)
  ms=$((t1 - t0))
  after=$(front)
  [ $rc -eq 0 ] || bad="$bad [tidyctl security exit $rc]"
  [ "$ms" -lt 20000 ] || bad="$bad [tidyctl security took ${ms} ms]"
  [ "$sw" -eq 0 ] || bad="$bad [softwareupdate was running]"
  [ "$before" = "$after" ] || bad="$bad [frontmost app changed: $before -> $after]"
  tail -n 1 "$out" | jq -e '.event == "security"' >/dev/null 2>&1 || bad="$bad [last line is not the security report]"
  tail -n 1 "$out" | jq -e '
      ([.checks[].id] == ["filevault", "firewall", "gatekeeper", "sip", "xprotect", "macos_updates", "login_items", "chrome_extensions"])
      and all(.checks[]; (.status | IN("green", "amber", "gray")) and (.sentence | length > 0))
      and ([.checks[] | select(.id == "macos_updates")][0].status == "gray")
      and .updates_checked == false' \
    >/dev/null 2>&1 || bad="$bad [the checks differ from design 5.4: $(tail -n 1 "$out" | jq -c '[.checks[] | {id, status}]' 2>/dev/null)]"
  local id
  id=$(tail -n 1 "$out" | jq -r '.id // empty' 2>/dev/null)
  if [ -n "$id" ] && [ -s "$sec_home/data/security/$id.json" ]; then
    detail="$detail tidyctl-security(${ms}ms)"
  else
    bad="$bad [the run file $sec_home/data/security/$id.json was not saved]"
  fi
  # A first run in a fresh TIDY_HOME says so and marks nothing.
  tail -n 1 "$out" | jq -e '.first_run == true' >/dev/null 2>&1 || bad="$bad [the first run did not say it is the first]"

  # The next run (ids are whole seconds, so wait one) is saved next to the first and compares
  # with it. Login items can start or stop in between, so the marks themselves are not checked.
  sleep 1
  env TIDY_HOME="$sec_home" "$TIDYCTL" security >"$RUN/security2.out" 2>"$RUN/security2.err"
  rc=$?
  [ $rc -eq 0 ] || bad="$bad [second tidyctl security exit $rc]"
  tail -n 1 "$RUN/security2.out" | jq -e --arg id "$id" '.event == "security" and .first_run == false and .compared_to == $id' \
    >/dev/null 2>&1 || bad="$bad [second run did not compare with the first]"
  [ "$(find "$sec_home/data/security" -name '*.json' | wc -l | tr -d ' ')" = 2 ] || bad="$bad [expected two saved runs]"

  if [ -z "$bad" ]; then
    pass AC7 "security:$detail"
  else
    fail AC7 "security:$detail$bad"
  fi
}

# ---------------------------------------------------------------------------------------------
# AC8: Check up and the weekly reminder
# ---------------------------------------------------------------------------------------------

ac8() {
  clear_tidy_env
  local bad="" detail="" rc out="$RUN/checkup.out"
  if [ ! -x "$TIDYCTL" ] || [ ! -x "$FIXTURE_BIN" ]; then
    fail AC8 "missing $TIDYCTL or $FIXTURE_BIN"
    return
  fi

  # Reminder math with a FixedClock: 10:00 local, 7 days after the last check up, rescheduled
  # after a check up and on launch, holding across both DST changes.
  if run_named reminder-core "-p tidy-core --lib" \
    seven_days_after_the_last_check_up_at_ten \
    the_local_date_counts_not_the_utc_date \
    holds_ten_o_clock_across_the_autumn_change \
    holds_ten_o_clock_across_the_spring_change \
    a_new_check_up_and_a_launch_reschedule \
    never_in_the_past; then
    detail="$detail reminder-math"
  else
    bad="$bad [reminder math tests failed, see $LAST_LOG]"
  fi
  if run_named checkup-core "-p tidy-core --test checkup" \
    a_check_up_of_the_fixture_home_logs_one_entry_with_the_three_parts \
    a_later_check_up_appends_compares_with_the_last_and_moves_the_reminder; then
    detail="$detail core-tests"
  else
    bad="$bad [check up core tests failed, see $LAST_LOG]"
  fi

  # App tests with a fake notifier: permission only when the toggle goes on, never at launch,
  # and a denied permission shows the Notifications settings button.
  if run_named checkup-app "-p tidy-app" \
    permission_is_not_asked_at_launch_or_when_visiting_check_up \
    switching_the_reminder_on_asks_once_then_schedules_the_single_notification \
    at_launch_with_the_reminder_on_it_only_looks_up_and_reschedules \
    at_launch_with_permission_not_yet_given_nothing_is_asked_or_scheduled \
    a_denied_permission_shows_the_notifications_settings_button \
    run_check_up_sends_one_command_with_the_updates_choice \
    the_three_cards_fill_in_and_link_to_their_screens; then
    detail="$detail app-tests"
  else
    bad="$bad [check up app tests failed, see $LAST_LOG]"
  fi

  # tidyctl checkup on the fixture home. The status reads (memory, Security) are the real,
  # read-only ones; softwareupdate does not run.
  if ! make_fixture fx-checkup; then
    fail AC8 "could not build the fixture"
    return
  fi
  local tz="PST8PDT,M3.2.0,M11.1.0"
  fx_run env TIDY_TEST_TZ="$tz" "$TIDYCTL" checkup >"$out" 2>"$RUN/checkup.err"
  rc=$?
  [ $rc -eq 0 ] || bad="$bad [checkup exit $rc, see $RUN/checkup.err]"
  jq -s -e 'length > 0 and all(.[]; type == "object" and has("event"))' "$out" >/dev/null 2>&1 ||
    bad="$bad [a line is not a JSON event]"
  local ev
  for ev in plan security memory; do
    [ "$(jq -s --arg e "$ev" '[.[] | select(.event == $e)] | length' "$out" 2>/dev/null)" = 1 ] ||
      bad="$bad [not exactly one $ev line]"
  done
  tail -n 1 "$out" | jq -e '.event == "checkup"' >/dev/null 2>&1 || bad="$bad [last line is not the check up]"

  # The three parts: the cleanup total of the expected plan, the security summary, the verdict.
  local safe
  safe=$(jq '[.items[] | select(.ticked) | .bytes_alloc] | add' "$FX_DIR/expected-plan.json")
  tail -n 1 "$out" | jq -e --argjson safe "$safe" '
      .cleanup_total == $safe
      and (.security_summary | type == "string" and length > 0)
      and (.memory_verdict | type == "string" and startswith("Memory is"))' >/dev/null 2>&1 ||
    bad="$bad [the three parts are wrong: $(tail -n 1 "$out" | jq -c '{cleanup_total, security_summary, memory_verdict}' 2>/dev/null), expected cleanup $safe]"

  # One entry in checkups.jsonl with the three parts; a second run appends a second one.
  local log="$FX_TIDY/data/checkups.jsonl"
  if [ -s "$log" ] && [ "$(wc -l <"$log" | tr -d ' ')" = 1 ] &&
    jq -e '.cleanup and .security and .memory and (.at | type == "number")' "$log" >/dev/null 2>&1; then
    detail="$detail tidyctl-checkup"
  else
    bad="$bad [checkups.jsonl does not hold exactly one complete entry]"
  fi

  # The reminder: 10:00 on the day 7 days after the check up, in the fixed test zone.
  local at want
  at=$(tail -n 1 "$out" | jq -r '.checkup.at' 2>/dev/null)
  want=$(TZ=America/Los_Angeles date -r "$((at + 7 * 86400))" '+%-m %-d' 2>/dev/null)
  tail -n 1 "$out" | jq -e --arg want "$want" '"\(.reminder.month) \(.reminder.day)" == $want and .reminder.hour == 10 and .reminder.minute == 0' >/dev/null 2>&1 ||
    bad="$bad [the reminder is not at 10:00 on $want: $(tail -n 1 "$out" | jq -c .reminder 2>/dev/null)]"

  fx_run env TIDY_TEST_TZ="$tz" "$TIDYCTL" checkup >"$RUN/checkup2.out" 2>"$RUN/checkup2.err"
  rc=$?
  [ $rc -eq 0 ] || bad="$bad [second checkup exit $rc]"
  [ "$(wc -l <"$log" 2>/dev/null | tr -d ' ')" = 2 ] || bad="$bad [the second check up did not append]"
  # Nothing was cleaned by a check up.
  [ ! -e "$FX_TIDY/data/history.jsonl" ] || bad="$bad [a check up wrote history]"

  if [ -z "$bad" ]; then
    pass AC8 "check up:$detail"
  else
    fail AC8 "check up:$detail$bad"
  fi
}

# ---------------------------------------------------------------------------------------------
# AC9: the app, sidebar and screenshots of all five screens
# ---------------------------------------------------------------------------------------------

ac9() {
  clear_tidy_env
  local bad="" detail=""

  if run_named app-screens "-p tidy-app" \
    the_sidebar_lists_five_screens_in_order_and_cmd_1_to_5_switch \
    long_paths_in_large_files_are_fitted_to_the_row \
    cmd_r_rescans_each_new_screen; then
    detail="$detail sidebar-and-paths"
  else
    bad="$bad [app tests failed, see $LAST_LOG]"
  fi

  if [ -n "${ACCEPT_SKIP_BUNDLE:-}" ]; then
    if [ -z "$bad" ]; then
      skip AC9 "ACCEPT_SKIP_BUNDLE is set: the shots of the five screens were not made ($detail ran)"
    else
      fail AC9 "app:$detail$bad"
    fi
    return
  fi

  if ! run_logged shots-bundle-m2 "$REPO/scripts/shots-bundle.sh"; then
    fail AC9 "shots-bundle.sh failed, see $LAST_LOG$bad"
    return
  fi
  local stamp="$RUN/shots-m2.stamp" out="${OUT:-$REPO/docs/app-shots}" before after
  : >"$stamp"
  sleep 1
  before=$(front)
  if run_logged shots-m2 env SHOTS_RENDER=1 "$REPO/scripts/shots.sh" checkup memory security cleanup large; then
    detail="$detail shots"
  else
    bad="$bad [shots.sh failed, see $LAST_LOG]"
  fi
  after=$(front)
  [ "$before" = "$after" ] || bad="$bad [frontmost app changed: $before -> $after]"
  local f w h s sums="" dups scr
  for scr in checkup cleanup large memory security; do
    for s in light dark; do
      f="$scr-$s"
      if [ ! -s "$out/$f.png" ] || [ ! "$out/$f.png" -nt "$stamp" ]; then
        bad="$bad [$f.png was not written by this run]"
        continue
      fi
      w=$(sips -g pixelWidth "$out/$f.png" | awk '/pixelWidth/{print $2}')
      h=$(sips -g pixelHeight "$out/$f.png" | awk '/pixelHeight/{print $2}')
      [ "${w}x${h}" = "1800x1240" ] || bad="$bad [$f.png is ${w}x${h}]"
      sums="$sums$(shasum "$out/$f.png" | awk '{print $1}')
"
    done
  done
  # Ten different pictures: a wrong scenario name or a stuck appearance would repeat one.
  dups=$(printf '%s' "$sums" | sort | uniq -d | wc -l | tr -d ' ')
  [ "$dups" = 0 ] || bad="$bad [two shots are identical]"

  if [ -z "$bad" ]; then
    pass AC9 "app:$detail; 10 shots at 1800x1240; frontmost app unchanged"
  else
    fail AC9 "app:$detail$bad"
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
ac6
ac7
ac8
ac9

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
