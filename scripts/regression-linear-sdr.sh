#!/usr/bin/env bash
# Automated checks for the linear SDR compositing path.
# Visual items (wallpaper, dual-head, popups) still need a manual pass — see checklist at end.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass() { echo -e "${GREEN}PASS${NC} $*"; }
fail() { echo -e "${RED}FAIL${NC} $*"; exit 1; }
warn() { echo -e "${YELLOW}WARN${NC} $*"; }
section() { echo; echo "== $* =="; }

section "Build + unit tests"
cargo test -p focaldesk-engine color:: --quiet
pass "color unit tests"

cargo build -p focaldesk-desktop -p focaldesk-color-tag-test --quiet
pass "focaldesk-desktop + focaldesk-color-tag-test build"

section "Session logs (current boot)"
if ! journalctl --user -b -q -g 'Linear SDR probe' --no-pager 2>/dev/null | grep -q .; then
    warn "No 'Linear SDR probe' lines — compositor may not be running this boot"
else
    journalctl --user -b -g 'Linear SDR probe' --no-pager | tail -5
    pass "Linear SDR startup probe present"
fi

if journalctl --user -b -q -g 'Linear SDR disabled' --no-pager 2>/dev/null | grep -q .; then
    journalctl --user -b -g 'Linear SDR disabled' --no-pager
    fail "Linear SDR was disabled at runtime (see above)"
fi
pass "No Linear SDR disable messages"

if journalctl --user -b -q -g 'compile failed' --no-pager 2>/dev/null \
    | rg -i 'shader|linear|srgb|composite' | grep -q .; then
    journalctl --user -b -g 'compile failed' --no-pager | rg -i 'shader|linear|srgb|composite'
    fail "Shader compile failure in session logs"
fi
pass "No linear/shader compile failures in logs"

COLOR_TAGS=$(journalctl --user -b -q -g 'color tag applied' --no-pager 2>/dev/null | wc -l || true)
if [[ "$COLOR_TAGS" -eq 0 ]]; then
    warn "No 'color tag applied' lines yet — run focaldesk-color-tag-test to exercise tags"
else
    pass "color tag applied ($COLOR_TAGS log lines this boot)"
fi

section "Optional: color tag client"
if [[ "${RUN_COLOR_TAG_TEST:-0}" == "1" ]]; then
    DISPLAY="${WAYLAND_DISPLAY:-focaldesk-0}"
    BIN="$ROOT/target/debug/focaldesk-color-tag-test"
    echo "Launching $BIN on WAYLAND_DISPLAY=$DISPLAY (close window to continue)..."
    WAYLAND_DISPLAY="$DISPLAY" "$BIN" --transfer srgb
    WAYLAND_DISPLAY="$DISPLAY" "$BIN" --transfer linear
    journalctl --user -b -g 'color tag applied' --no-pager | tail -5
    pass "color tag test client completed"
else
    echo "Set RUN_COLOR_TAG_TEST=1 to run focaldesk-color-tag-test (srgb + linear)."
fi

section "Manual checklist (visual)"
cat <<'EOF'
  [ ] Linear default: full wallpaper both outputs, no diagonal black wedge
  [ ] Glass + chrome + cursor + popups look correct
  [ ] Drag window across outputs (DP-3 / DP-4)
  [ ] FOCALDESK_LINEAR_SDR=0 restart: legacy path still OK
  [ ] focaldesk-color-tag-test --transfer srgb|linear (or RUN_COLOR_TAG_TEST=1)
EOF

echo
pass "Automated regression checks complete"
