#!/usr/bin/env bash
# Capture one live Fynd USDT -> WETH quote for each consecutive Ethereum block.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ -f "$REPO_ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$REPO_ROOT/.env"
  set +a
fi

COUNT="${1:-20}"
AMOUNT_USDT="${2:-100000}"
FYND_URL="${FYND_URL:-http://127.0.0.1:3001}"
SENDER="${SENDER:-0x000000000000000000000000000000000000BEEF}"
OUTPUT="${OUTPUT:-artifacts/benchmarks/runs/usdt-weth-${COUNT}-blocks.jsonl}"

command -v jq >/dev/null || { echo "error: jq is required" >&2; exit 1; }
curl -sf "$FYND_URL/v1/health" >/dev/null || {
  echo "error: Fynd is not healthy at $FYND_URL" >&2
  exit 1
}

AMOUNT_ATOMIC="$(python3 - "$AMOUNT_USDT" <<'PY'
from decimal import Decimal, ROUND_DOWN
import sys
print((Decimal(sys.argv[1]) * Decimal(1_000_000)).to_integral_value(rounding=ROUND_DOWN))
PY
)"

mkdir -p "$(dirname "$OUTPUT")"
touch "$OUTPUT"
: >"$OUTPUT"

captured=0
last_block=0
while (( captured < COUNT )); do
  response="$(curl -sf -X POST "$FYND_URL/v1/quote" \
    -H 'Content-Type: application/json' \
    -d "{\"orders\":[{\"token_in\":\"0xdAC17F958D2ee523a2206206994597C13D831ec7\",\"token_out\":\"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2\",\"amount\":\"$AMOUNT_ATOMIC\",\"side\":\"sell\",\"sender\":\"$SENDER\"}],\"options\":{\"timeout_ms\":5000,\"min_responses\":1}}")"
  block="$(jq -r '.orders[0].block.number // 0' <<<"$response")"
  status="$(jq -r '.orders[0].status // "unknown"' <<<"$response")"
  if (( block > last_block )); then
    jq -c --arg amount_usdt "$AMOUNT_USDT" \
      '{captured_at: now | todateiso8601, amount_usdt: $amount_usdt,
        solve_time_ms, quote: .orders[0]}' <<<"$response" >>"$OUTPUT"
    captured=$((captured + 1))
    last_block="$block"
    gross="$(jq -r '.orders[0].amount_out' <<<"$response")"
    net="$(jq -r '.orders[0].amount_out_net_gas' <<<"$response")"
    echo "[$captured/$COUNT] block=$block status=$status gross=$gross net=$net"
  fi
  (( captured == COUNT )) || sleep 2
done

echo "wrote $OUTPUT"
