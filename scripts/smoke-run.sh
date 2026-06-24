#!/usr/bin/env bash
# ABOUTME: Builds and briefly launches the native Aether Monitor binary.
# ABOUTME: Stops the AppKit run loop process after a bounded smoke interval.

set -euo pipefail

cargo build

"$(pwd)/target/debug/aether_monitor" &
app_pid="$!"

sleep "${AETHER_MONITOR_SMOKE_SECONDS:-3}"
kill "$app_pid"
wait "$app_pid" 2>/dev/null || true
