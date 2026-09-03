#!/usr/bin/env bash
# Quote one live Ethereum USDT -> WETH user order through the hybrid V2/V3 router.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

AMOUNT_USDT="${1:?usage: $0 <USDT amount, e.g. 100000>}"
FYND_PORT="${FYND_PORT:-3001}"
FYND_URL="${FYND_URL:-http://127.0.0.1:$FYND_PORT}"
TYCHO_URL="${TYCHO_URL:?set TYCHO_URL, e.g. tycho-fynd-ethereum.propellerheads.xyz}"
TYCHO_API_KEY="${TYCHO_API_KEY:?set TYCHO_API_KEY}"
RPC_URL="${RPC_URL:?set RPC_URL}"
SENDER="${SENDER:-0x000000000000000000000000000000000000BEEF}"
LOG="target/hybrid-usdt-weth-$FYND_PORT.log"
STARTED_SERVER=0

AMOUNT_ATOMIC="$(python3 - "$AMOUNT_USDT" <<'PY'
from decimal import Decimal, InvalidOperation, ROUND_DOWN
import sys

try:
    amount = Decimal(sys.argv[1])
except InvalidOperation:
    raise SystemExit("USDT amount must be a decimal number")
if amount <= 0:
    raise SystemExit("USDT amount must be positive")
print((amount * Decimal(1_000_000)).to_integral_value(rounding=ROUND_DOWN))
PY
)"

cleanup() {
  if (( STARTED_SERVER )); then
    kill "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if ! curl -sf "$FYND_URL/v1/health" >/dev/null; then
  mkdir -p target
  echo "Starting hybrid Fynd on $FYND_URL (log: $LOG)"
  cargo run --release -p fynd -- serve \
    --http-port "$FYND_PORT" \
    --tycho-url "$TYCHO_URL" \
    --tycho-api-key "$TYCHO_API_KEY" \
    --rpc-url "$RPC_URL" \
    --protocols uniswap_v2,uniswap_v3 \
    --min-tvl 10 \
    --worker-pools-config scripts/exact-v2-worker-pools.toml >"$LOG" 2>&1 &
  SERVER_PID=$!
  STARTED_SERVER=1
  for _ in $(seq 1 180); do
    curl -sf "$FYND_URL/v1/health" >/dev/null && break
    sleep 1
  done
  curl -sf "$FYND_URL/v1/health" >/dev/null || {
    echo "error: hybrid Fynd did not become healthy; see $LOG" >&2
    exit 1
  }
fi

RESPONSE="$(curl -sf -X POST "$FYND_URL/v1/quote" \
  -H 'Content-Type: application/json' \
  -d "{\"orders\":[{\"token_in\":\"0xdAC17F958D2ee523a2206206994597C13D831ec7\",\"token_out\":\"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2\",\"amount\":\"$AMOUNT_ATOMIC\",\"side\":\"sell\",\"sender\":\"$SENDER\"}],\"options\":{\"timeout_ms\":5000,\"min_responses\":1}}")"

QUOTE_RESPONSE="$RESPONSE" python3 - "$AMOUNT_USDT" <<'PY'
import json
import os
import sys
from decimal import Decimal

amount_in = sys.argv[1]
quote = json.loads(os.environ["QUOTE_RESPONSE"])
order = quote["orders"][0]
if order.get("status") != "success":
    raise SystemExit(f"no successful route: {order.get('status')}")

def eth(wei):
    return Decimal(wei) / Decimal(10**18)

print(f"\nHybrid live quote: {amount_in} USDT -> WETH (1 WETH is redeemable for 1 ETH)")
print(f"gross output: {eth(order['amount_out']):.18f} WETH")
print(f"net output:   {eth(order['amount_out_net_gas']):.18f} WETH")
print(f"gas estimate: {order['gas_estimate']} gas")
print(f"solve time:   {quote.get('solve_time_ms', 'unknown')} ms")

route = order.get("route") or {}
swaps = route.get("swaps") or []
print(f"route legs:   {len(swaps)}")
for index, swap in enumerate(swaps, 1):
    print(f"  {index}. {swap.get('protocol', 'unknown')} {swap.get('component_id', swap.get('pool', 'unknown'))}")
PY
