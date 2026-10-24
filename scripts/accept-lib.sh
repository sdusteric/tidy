#!/usr/bin/env bash
# Shared helpers for scripts/accept.sh. Source it, do not run it.
#
# * ACCEPT_RUN_DIR (default target/accept/<timestamp>) holds the logs and the results file.
#   Nothing here touches the real Tidy folders, the real home folder or the real Trash.
# * record/pass/fail/skip print one line per criterion and append it to results.tsv.
# * track_pid registers a process this run started; the EXIT trap stops each one by PID.
# * remove_run_path deletes only exact absolute paths below target/accept/.
#
# Written for bash 3.2 (macOS /bin/bash): no associative arrays, no `set -u`.

if [ -n "${ACCEPT_LIB_LOADED:-}" ]; then
  return 0 2>/dev/null || true
fi
ACCEPT_LIB_LOADED=1

ACCEPT_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$ACCEPT_LIB_DIR/.." && pwd -P)"
cd "$REPO" || exit 1

if [ -z "${ACCEPT_RUN_DIR:-}" ]; then
  ACCEPT_RUN_DIR="$REPO/target/accept/$(date +%Y%m%d-%H%M%S)"
fi
mkdir -p "$ACCEPT_RUN_DIR"
ACCEPT_RUN_DIR="$(cd "$ACCEPT_RUN_DIR" && pwd -P)"
export ACCEPT_RUN_DIR
RUN="$ACCEPT_RUN_DIR"
RESULTS="$RUN/results.tsv"
touch "$RESULTS"

# ---------------------------------------------------------------------------------------------
# Results
# ---------------------------------------------------------------------------------------------

record() { # id status note
  printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$RESULTS"
  printf '%-14s %-5s %s\n' "$1" "$2" "$3"
}
pass() { record "$1" PASS "${2:-}"; }
fail() { record "$1" FAIL "${2:-}"; }
skip() { record "$1" SKIP "${2:-}"; }

note() { printf '  %s\n' "$*" >&2; }

# ---------------------------------------------------------------------------------------------
# Commands with logs
# ---------------------------------------------------------------------------------------------

LAST_LOG=""

# run_logged <name> <command...>: output goes to $RUN/<name>.log. Shows the tail on failure.
run_logged() {
  local name=$1
  shift
  LAST_LOG="$RUN/$name.log"
  note "$name: $*"
  "$@" >"$LAST_LOG" 2>&1
  local rc=$?
  if [ $rc -ne 0 ]; then
    note "$name failed (exit $rc), last lines of $LAST_LOG:"
    tail -n 12 "$LAST_LOG" | sed 's/^/    /' >&2
  fi
  return $rc
}

# ---------------------------------------------------------------------------------------------
# Processes this run started
# ---------------------------------------------------------------------------------------------

TRACKED_PIDS=""

track_pid() { TRACKED_PIDS="$TRACKED_PIDS $1"; }

pid_alive() { kill -0 "$1" 2>/dev/null; }

# stop_pid <pid>: SIGTERM, up to 5 s of grace, then SIGKILL. Only ever a PID we started.
stop_pid() {
  local pid=$1 i
  pid_alive "$pid" || return 0
  kill -TERM "$pid" 2>/dev/null
  for i in 1 2 3 4 5 6 7 8 9 10; do
    pid_alive "$pid" || return 0
    sleep 0.5
  done
  kill -KILL "$pid" 2>/dev/null
  return 0
}

accept_cleanup() {
  local p
  for p in $TRACKED_PIDS; do
    stop_pid "$p"
  done
  TRACKED_PIDS=""
  if [ -n "${ACCEPT_EXTRA_CLEANUP:-}" ]; then
    $ACCEPT_EXTRA_CLEANUP
  fi
}
trap accept_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ---------------------------------------------------------------------------------------------
# Deleting
# ---------------------------------------------------------------------------------------------

# remove_run_path <absolute path>: recursive delete of one exact directory below target/accept/.
remove_run_path() {
  local p=$1
  case "$p" in
    "$REPO"/target/accept/?*) ;;
    *)
      note "refusing to delete $p"
      return 1
      ;;
  esac
  case "$p" in
    *..* | *"*"* | *"?"*)
      note "refusing to delete $p"
      return 1
      ;;
  esac
  [ -d "$p" ] || return 0
  rm -rf -- "$p"
}

# ---------------------------------------------------------------------------------------------
# Environment
# ---------------------------------------------------------------------------------------------

# Forget every TIDY_* variable from the caller's shell so no hook leaks into a test.
clear_tidy_env() {
  local v
  for v in $(env | sed -n 's/^\(TIDY_[A-Za-z0-9_]*\)=.*/\1/p'); do
    unset "$v"
  done
}
