#!/usr/bin/env bash
# Compare Lexion with Uniswap SOR against one immutable V2/V3/V4 Ethereum snapshot.
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
OUTPUT="${2:-artifacts/benchmarks/comparisons/uniswap/lexion-vs-uniswap-pinned-${ORDERS}.json}"
UPSTREAM_RPC="${UNISWAP_RPC_URL:-${RPC_URL:-}}"
FYND_PORT="${LEXION_COMPARE_PORT:-3011}"
FORK_PORT="${LEXION_FORK_PORT:-8547}"
FYND_URL="http://127.0.0.1:${FYND_PORT}"
FORK_RPC="http://127.0.0.1:${FORK_PORT}"
SERVER_LOG="target/lexion-uniswap-pinned-server.log"
ANVIL_LOG="target/lexion-uniswap-pinned-anvil.log"

if [[ -z "$UPSTREAM_RPC" ]]; then
  echo "error: set UNISWAP_RPC_URL or RPC_URL to an archive-capable Ethereum RPC" >&2
  exit 1
fi
: "${TYCHO_API_KEY:?set TYCHO_API_KEY}"
: "${TYCHO_URL:?set TYCHO_URL}"

mkdir -p "$(dirname "$OUTPUT")" target

cleanup() {
  [[ -z "${SERVER_PID:-}" ]] || kill "$SERVER_PID" 2>/dev/null || true
  [[ -z "${ANVIL_PID:-}" ]] || kill "$ANVIL_PID" 2>/dev/null || true
}
trap cleanup EXIT

echo "Building benchmark and router binaries ..."
cargo build --release -p fynd -p fynd-benchmark

echo "Starting a dedicated V2/V3/V4 Lexion instance ..."
target/release/fynd serve \
  --http-host 127.0.0.1 \
  --http-port "$FYND_PORT" \
  --tycho-url "$TYCHO_URL" \
  --tycho-api-key "$TYCHO_API_KEY" \
  --rpc-url "$UPSTREAM_RPC" \
  --protocols uniswap_v2,uniswap_v3,uniswap_v4 \
  --min-tvl 10 \
  --worker-pools-config scripts/lexion-worker-pools.toml >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 180); do
  curl -sf "$FYND_URL/v1/health" >/dev/null && break
  sleep 1
done
curl -sf "$FYND_URL/v1/health" >/dev/null || {
  echo "error: Lexion did not become healthy; see $SERVER_LOG" >&2
  exit 1
}

echo "Capturing Lexion quotes and executable calldata ..."
target/release/fynd-benchmark audit \
  --fynd-url "$FYND_URL" \
  --fynd-only \
  --random-orders "$ORDERS" \
  --blocks 1 \
  --top-pairs 25 \
  --exclude-native \
  --trade-data tools/fynd-gas-audit/out/aggregator_trades_10k.json \
  --concurrency 1 \
  --rpc-url "$UPSTREAM_RPC" \
  --output "$OUTPUT"

BLOCK_HASH="$(jq -r '[.results[].block_hash | select(. != null)][0] // empty' "$OUTPUT")"
if [[ -z "$BLOCK_HASH" ]]; then
  echo "error: audit contains no pinned block hash" >&2
  exit 1
fi
BLOCK_NUMBER="$(cast block "$BLOCK_HASH" --rpc-url "$UPSTREAM_RPC" --field number)"

echo "Starting immutable Ethereum fork at block $BLOCK_NUMBER ..."
anvil \
  --fork-url "$UPSTREAM_RPC" \
  --fork-block-number "$BLOCK_NUMBER" \
  --port "$FORK_PORT" \
  --silent >"$ANVIL_LOG" 2>&1 &
ANVIL_PID=$!
for _ in $(seq 1 60); do
  cast block-number --rpc-url "$FORK_RPC" >/dev/null 2>&1 && break
  sleep 1
done
cast block-number --rpc-url "$FORK_RPC" >/dev/null || {
  echo "error: Anvil did not become healthy; see $ANVIL_LOG" >&2
  exit 1
}

npm install --prefix tools/uniswap-router-bench --no-audit --no-fund --package-lock=false >/dev/null
echo "Routing and estimating both transactions on the pinned fork ..."
UNISWAP_RPC_URL="$FORK_RPC" npx --yes node@20 \
  tools/uniswap-router-bench/normalized-audit.cjs "$OUTPUT"
