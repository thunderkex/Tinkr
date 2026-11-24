#!/usr/bin/env sh
# Cross-platform install + end-to-end verification script (macOS / Linux).
set -eu
TINKR="${1:-./target/release/tinkr}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export TINKR_HOME="${TMP}/tinkr-home"

"$TINKR" --version
"$TINKR" doctor
"$TINKR" run --shell sh -- "ls -la crates/tinkr-core/src"
"$TINKR" gain
"$TINKR" undo --purge
echo "E2E OK"
