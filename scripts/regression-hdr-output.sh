#!/usr/bin/env bash
# Measure the SDR white anchor and HDR highlights while HDR10 output is active.
set -euo pipefail

CLI="${FOCALDESK_CLI:-focaldesk-cli}"
SPOTREAD="${SPOTREAD_BIN:-spotread}"
CONNECTOR=""
SETTLE_SECONDS="${FOCALDESK_HDR_TEST_SETTLE_SECONDS:-3}"
LUMINANCE_TOLERANCE_PERCENT="${FOCALDESK_HDR_TEST_LUMINANCE_TOLERANCE_PERCENT:-15}"
CHROMATICITY_TOLERANCE="${FOCALDESK_HDR_TEST_CHROMATICITY_TOLERANCE:-0.015}"
REPORT=""
SPOTREAD_ARGS=()
RESULT_ROWS=()
PATTERN_ACTIVE=0
SPOTREAD_READY=0

usage() {
    cat <<'EOF'
Usage: regression-hdr-output.sh --connector CONNECTOR [options]

Options:
  --spotread-arg ARG       Pass one argument to spotread (repeatable; e.g. -y, l)
  --settle-seconds N       Wait after changing each patch (default: 3)
  --luminance-tolerance P  Allowed luminance error in percent (default: 15)
  --chromaticity-tolerance V
                           Allowed absolute x/y error from D65 (default: 0.015)
  --report PATH            Write tab-separated measurements to PATH
  --self-test              Test the output parser and threshold checks only

The meter must be centered on the selected display. The test reads FocalDesk's
live HDR appearance targets, measures SDR/graphics white, the 10% HDR peak,
and the sustained full-frame HDR level, then restores the desktop.
EOF
}

die() {
    printf 'FAIL %s\n' "$*" >&2
    exit 1
}

parse_yxy() {
    sed -nE 's/.*Yxy:[[:space:]]*([-+0-9.eE]+)[,[:space:]]+([-+0-9.eE]+)[,[:space:]]+([-+0-9.eE]+).*/\1 \2 \3/p' \
        | tail -n 1
}

within_percent() {
    awk -v actual="$1" -v expected="$2" -v tolerance="$3" 'BEGIN {
        delta = actual - expected; if (delta < 0) delta = -delta
        limit = expected * tolerance / 100.0
        exit !(delta <= limit)
    }'
}

within_delta() {
    awk -v actual="$1" -v expected="$2" -v tolerance="$3" 'BEGIN {
        delta = actual - expected; if (delta < 0) delta = -delta
        exit !(delta <= tolerance)
    }'
}

greater_than_ratio() {
    awk -v high="$1" -v low="$2" -v ratio="$3" 'BEGIN { exit !(high >= low * ratio) }'
}

self_test() {
    local parsed
    parsed="$(printf '%s\n' 'Result is XYZ: 190.1 203.0 221.2, Yxy: 203.000 0.3127 0.3290' | parse_yxy)"
    [[ "$parsed" == "203.000 0.3127 0.3290" ]] || die "spotread Yxy parser"
    within_percent 200 203 5 || die "luminance in-range check"
    if within_percent 150 203 5; then die "luminance out-of-range check"; fi
    within_delta 0.3127 0.3127 0.001 || die "chromaticity in-range check"
    greater_than_ratio 450 203 1.25 || die "HDR separation check"
    printf 'PASS parser and threshold self-tests\n'
}

cleanup() {
    if (( PATTERN_ACTIVE )); then
        "$CLI" hdr-calibration-pattern "$CONNECTOR" off >/dev/null 2>&1 || \
            printf 'WARN could not restore the calibration pattern to off\n' >&2
    fi
}

measure_pattern() {
    local label="$1" pattern="$2" expected="$3" output parsed y x chroma_y
    printf 'MEASURE %s: switching to %s (target %.3f cd/m^2)\n' "$label" "$pattern" "$expected" >&2
    "$CLI" hdr-calibration-pattern "$CONNECTOR" "$pattern"
    PATTERN_ACTIVE=1
    sleep "$SETTLE_SECONDS"

    if (( SPOTREAD_READY )); then
        output="$("$SPOTREAD" -e -x -O -N "${SPOTREAD_ARGS[@]}" 2>&1)" || {
            printf '%s\n' "$output" >&2
            die "spotread failed while measuring $label"
        }
    else
        output="$("$SPOTREAD" -e -x -O "${SPOTREAD_ARGS[@]}" 2>&1)" || {
            printf '%s\n' "$output" >&2
            die "spotread failed while initializing for $label"
        }
    fi
    parsed="$(printf '%s\n' "$output" | parse_yxy)"
    if [[ -z "$parsed" ]]; then
        # Some instruments use the first -O invocation only for calibration.
        output="$("$SPOTREAD" -e -x -O -N "${SPOTREAD_ARGS[@]}" 2>&1)" || {
            printf '%s\n' "$output" >&2
            die "spotread calibration/measurement failed for $label"
        }
        parsed="$(printf '%s\n' "$output" | parse_yxy)"
    fi
    [[ -n "$parsed" ]] || {
        printf '%s\n' "$output" >&2
        die "spotread returned no Yxy result for $label"
    }
    read -r y x chroma_y <<<"$parsed"
    SPOTREAD_READY=1

    within_percent "$y" "$expected" "$LUMINANCE_TOLERANCE_PERCENT" || \
        die "$label luminance $y cd/m^2 is outside ${LUMINANCE_TOLERANCE_PERCENT}% of $expected"
    within_delta "$x" 0.3127 "$CHROMATICITY_TOLERANCE" || \
        die "$label x chromaticity $x is outside D65 tolerance"
    within_delta "$chroma_y" 0.3290 "$CHROMATICITY_TOLERANCE" || \
        die "$label y chromaticity $chroma_y is outside D65 tolerance"

    RESULT_ROWS+=("$label"$'\t'"$pattern"$'\t'"$expected"$'\t'"$y"$'\t'"$x"$'\t'"$chroma_y")
    printf 'PASS %s: Y=%s cd/m^2 x=%s y=%s\n' "$label" "$y" "$x" "$chroma_y"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit 0
fi

while (($#)); do
    case "$1" in
        --connector) CONNECTOR="${2:-}"; shift 2 ;;
        --spotread-arg) SPOTREAD_ARGS+=("${2:-}"); shift 2 ;;
        --spotread-arg=*) SPOTREAD_ARGS+=("${1#*=}"); shift ;;
        --settle-seconds) SETTLE_SECONDS="${2:-}"; shift 2 ;;
        --luminance-tolerance) LUMINANCE_TOLERANCE_PERCENT="${2:-}"; shift 2 ;;
        --chromaticity-tolerance) CHROMATICITY_TOLERANCE="${2:-}"; shift 2 ;;
        --report) REPORT="${2:-}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
done

[[ -n "$CONNECTOR" ]] || { usage >&2; die "--connector is required"; }
command -v "$CLI" >/dev/null || die "focaldesk-cli not found (set FOCALDESK_CLI)"
command -v "$SPOTREAD" >/dev/null || die "spotread not found (set SPOTREAD_BIN)"
command -v python3 >/dev/null || die "python3 is required to read runtime status"
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

runtime_json="$("$CLI" display-runtime-status)" || die "could not read display runtime status"
runtime_values="$(printf '%s' "$runtime_json" | python3 -c '
import json, sys
connector = sys.argv[1]
for output in json.load(sys.stdin):
    if output.get("connector") == connector:
        appearance = output.get("hdr_appearance", {})
        print("1" if output.get("hdr_active") else "0")
        print(appearance.get("reference_white_nits", 203.0))
        print(appearance.get("peak_nits", 450.0))
        print(appearance.get("full_frame_peak_nits", 300.0))
        break
else:
    raise SystemExit(f"display connector not found: {connector}")
' "$CONNECTOR")" || die "could not resolve HDR state for $CONNECTOR"
mapfile -t runtime_fields <<<"$runtime_values"
[[ "${runtime_fields[0]:-0}" == "1" ]] || die "HDR is not active on $CONNECTOR"
reference_nits="${runtime_fields[1]}"
peak_nits="${runtime_fields[2]}"
full_frame_nits="${runtime_fields[3]}"

# Setting the first stimulus also makes the compositor independently reject a
# stale runtime status: non-off patterns are accepted only on a live HDR output.
measure_pattern "sdr-reference-white" "reference-white" "$reference_nits"
reference_measured="$(cut -f4 <<<"${RESULT_ROWS[0]}")"
measure_pattern "hdr-10-percent-peak" "peak-window" "$peak_nits"
peak_measured="$(cut -f4 <<<"${RESULT_ROWS[1]}")"
measure_pattern "hdr-full-frame" "peak-full-frame" "$full_frame_nits"

greater_than_ratio "$peak_measured" "$reference_measured" 1.25 || \
    die "HDR peak $peak_measured cd/m^2 is less than 1.25x SDR white $reference_measured cd/m^2"
printf 'PASS HDR headroom: %s cd/m^2 is at least 1.25x SDR white %s cd/m^2\n' \
    "$peak_measured" "$reference_measured"

if [[ -n "$REPORT" ]]; then
    {
        printf 'label\tpattern\texpected_cd_m2\tmeasured_cd_m2\tx\ty\n'
        printf '%s\n' "${RESULT_ROWS[@]}"
    } >"$REPORT"
    printf 'PASS wrote report to %s\n' "$REPORT"
fi

printf 'PASS SDR and HDR output measurements on %s\n' "$CONNECTOR"
