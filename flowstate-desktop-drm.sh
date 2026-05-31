#!/usr/bin/env bash
set -euo pipefail

log=/tmp/flowstate.log
: > "$log"

cargo build -p flowstate-desktop --no-default-features --features "drm xwayland" 2>&1 | tee -a "$log"

RUST_LOG=trace target/debug/flowstate-desktop 2>&1 | tee -a "$log"
