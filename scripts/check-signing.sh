#!/usr/bin/env bash
# Signing gate for the built Tidy.app: hardened runtime, entitlements equal to
# resources/Tidy.entitlements, and every nested Mach-O signed by the same team. The checks live in
# app-updates/tools/check-signing.sh.
#
#   scripts/check-signing.sh [path/Tidy.app]
#
# Env: APP_UPDATES  the app-updates checkout (default: ../app-updates next to this repo)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_UPDATES="${APP_UPDATES:-$(cd "$REPO/.." && pwd)/app-updates}"
CHECK="$APP_UPDATES/tools/check-signing.sh"
[ -x "$CHECK" ] || { echo "missing $CHECK" >&2; exit 1; }
BUNDLE="${1:-${CARGO_TARGET_DIR:-$REPO/target}/bundle/Tidy.app}"
exec "$CHECK" "$BUNDLE" "$REPO/resources/Tidy.entitlements"
