#!/usr/bin/env bash
# Builds the screenshot-only bundle target/shots-build/bundle/Tidy.app: the release app with the
# `shot-export` feature, which lets a demo launch render its own first frame to a PNG
# (TIDY_DEMO_SHOT). Used by `SHOTS_RENDER=1 scripts/shots.sh`. Never launches the app, never
# installs it, and keeps its own target dir so the normal bundle (target/bundle) is untouched.
# It has its own bundle id (dev.sdusteric.tidy.shots): a demo launch then never shares an id
# with a Tidy.app the user runs, so LaunchServices cannot route anything between them.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TARGET="$REPO/target/shots-build"

(cd "$REPO" && CARGO_TARGET_DIR="$TARGET" cargo build -p tidy-app --release --features shot-export)
CARGO_TARGET_DIR="$TARGET" TIDY_BUNDLE_BIN="$TARGET/release/tidy" TIDY_BUNDLE_ID=dev.sdusteric.tidy.shots \
  "$REPO/scripts/bundle.sh" --release
