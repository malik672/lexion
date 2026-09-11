#!/usr/bin/env bash
# Runs the analysis-only Sacred Timeline cutoff sweep on one shared benchmark capture.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXPECTED_THRESHOLDS="0,0.01,0.1,0.5,1.0"
BENCH_ARGS=()

while (($#)); do
  case "$1" in
    --thresholds)
      if (($# < 2)); then
        echo "--thresholds requires a comma-separated value" >&2
        exit 2
      fi
      if [[ "$2" != "$EXPECTED_THRESHOLDS" ]]; then
        echo "this analysis build supports --thresholds $EXPECTED_THRESHOLDS" >&2
        exit 2
      fi
      shift 2
      ;;
    --thresholds=*)
      if [[ "${1#*=}" != "$EXPECTED_THRESHOLDS" ]]; then
        echo "this analysis build supports --thresholds $EXPECTED_THRESHOLDS" >&2
        exit 2
      fi
      shift
      ;;
    *)
      BENCH_ARGS+=("$1")
      shift
      ;;
  esac
done

exec "$SCRIPT_DIR/sacred-timeline-census.sh" "${BENCH_ARGS[@]}"
