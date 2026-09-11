#!/usr/bin/env bash
# Build a replayed Fynd audit and compare it with same-block Uniswap SOR transactions.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ -f "$REPO_ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$REPO_ROOT/.env"
  set +a
fi

ORDERS="${1:-30}"
OUTPUT="${2:-artifacts/benchmarks/comparisons/uniswap/fynd-vs-uniswap-normalized-${ORDERS}.json}"
RPC="${UNISWAP_RPC_URL:-${RPC_URL:-}}"
FYND_URL="${FYND_URL:-http://localhost:3000}"
SERVER_LOG="${FYND_NORMALIZED_SERVER_LOG:-target/fynd-uniswap-normalized-server.log}"
STARTED_SERVER=0

if [[ -z "$RPC" ]]; then
  echo "error: set UNISWAP_RPC_URL or RPC_URL to an archive-capable Ethereum RPC" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"

cleanup() {
  if (( STARTED_SERVER )); then
    kill "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if ! curl -sf "$FYND_URL/v1/health" >/dev/null; then
  : "${TYCHO_API_KEY:?set TYCHO_API_KEY to start the local Fynd server}"
  : "${TYCHO_URL:?set TYCHO_URL to start the local Fynd server}"
  mkdir -p "$(dirname "$SERVER_LOG")"
  echo "Starting Fynd at $FYND_URL (log: $SERVER_LOG) ..."
  cargo run --release -p fynd -- serve \
    --tycho-url "$TYCHO_URL" \
    --tycho-api-key "$TYCHO_API_KEY" \
    --rpc-url "$RPC" \
    --protocols uniswap_v2,uniswap_v3 \
    --min-tvl 10 \
    --worker-pools-config scripts/lexion-worker-pools.toml >"$SERVER_LOG" 2>&1 &
  SERVER_PID=$!
  STARTED_SERVER=1
  for _ in $(seq 1 180); do
    curl -sf "$FYND_URL/v1/health" >/dev/null && break
    sleep 1
  done
  curl -sf "$FYND_URL/v1/health" >/dev/null || {
    echo "error: Fynd did not become healthy; see $SERVER_LOG" >&2
    exit 1
  }
fi

cargo run -p fynd-benchmark --release -- audit \
  --fynd-url "$FYND_URL" \
  --fynd-only \
  --random-orders "$ORDERS" \
  --blocks 1 \
  --top-pairs 25 \
  --exclude-native \
  --trade-data tools/fynd-gas-audit/out/aggregator_trades_10k.json \
  --concurrency 1 \
  --rpc-url "$RPC" \
  --output "$OUTPUT"

npm install --prefix tools/uniswap-router-bench --no-audit --no-fund --package-lock=false >/dev/null
npx --yes node@20 tools/uniswap-router-bench/normalized-audit.cjs "$OUTPUT"
