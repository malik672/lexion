#!/usr/bin/env bash
# Run the normalized Lexion/Uniswap V2/V3/V4 comparison at exact future blocks.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ -f "$REPO_ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$REPO_ROOT/.env"
  set +a
fi

BLOCKS=""
ORDERS=30
OUTPUT_DIR="artifacts/benchmarks/comparisons/uniswap/by-block"
FYND_PORT="${LEXION_COMPARE_PORT:-3011}"
FORK_PORT="${LEXION_FORK_PORT:-8547}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --blocks) BLOCKS="$2"; shift 2 ;;
    --orders) ORDERS="$2"; shift 2 ;;
    --output-dir) OUTPUT_DIR="$2"; shift 2 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$BLOCKS" ]]; then
  echo "usage: $0 --blocks N[,N...]|+OFFSET[, +OFFSET...] [--orders N]" >&2
  exit 2
fi

UPSTREAM_RPC="${UNISWAP_RPC_URL:-${RPC_URL:-}}"
if [[ -z "$UPSTREAM_RPC" ]]; then
  echo "error: set UNISWAP_RPC_URL or RPC_URL" >&2
  exit 1
fi
: "${TYCHO_API_KEY:?set TYCHO_API_KEY}"
: "${TYCHO_URL:?set TYCHO_URL}"

FYND_URL="http://127.0.0.1:${FYND_PORT}"
FORK_RPC="http://127.0.0.1:${FORK_PORT}"
SERVER_LOG="target/lexion-uniswap-blocks-server.log"
ANVIL_LOG="target/lexion-uniswap-blocks-anvil.log"
mkdir -p "$OUTPUT_DIR" target

cleanup() {
  [[ -z "${SERVER_PID:-}" ]] || kill "$SERVER_PID" 2>/dev/null || true
  [[ -z "${ANVIL_PID:-}" ]] || kill "$ANVIL_PID" 2>/dev/null || true
}
trap cleanup EXIT

cargo build --release -p fynd -p fynd-benchmark
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

npm install --prefix tools/uniswap-router-bench --no-audit --no-fund --package-lock=false >/dev/null

IFS=',' read -r -a TARGET_BLOCKS <<< "$BLOCKS"
BASE_BLOCK="$(cast block-number --rpc-url "$UPSTREAM_RPC")"
CAPTURED_AUDITS=()
CAPTURED_BLOCKS=()
echo "Block reference after startup: $BASE_BLOCK"
for requested in "${TARGET_BLOCKS[@]}"; do
  requested="${requested//[[:space:]]/}"
  if [[ "$requested" =~ ^\+([0-9]+)$ ]]; then
    target=$((BASE_BLOCK + ${BASH_REMATCH[1]}))
    echo "Resolved $requested to block $target"
  elif [[ "$requested" =~ ^[0-9]+$ ]]; then
    target="$requested"
  else
    echo "error: invalid block number or relative offset: $requested" >&2
    exit 2
  fi
  current="$(cast block-number --rpc-url "$UPSTREAM_RPC")"
  if (( target <= current )); then
    echo "error: block $target is not in the future (current: $current)" >&2
    echo "Historical blocks require a previously captured Lexion audit artifact." >&2
    exit 1
  fi

  echo "Waiting for block $target (current: $current) ..."
  while (( current < target - 1 )); do
    sleep 1
    current="$(cast block-number --rpc-url "$UPSTREAM_RPC")"
  done

  audit="$OUTPUT_DIR/lexion-vs-uniswap-block-${target}.json"
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
    --output "$audit"

  block_hash="$(jq -r '[.results[].block_hash | select(. != null)][0] // empty' "$audit")"
  if [[ -z "$block_hash" ]]; then
    echo "error: trigger block $target produced no captured block hash" >&2
    exit 1
  fi
  captured="$(cast block "$block_hash" --rpc-url "$UPSTREAM_RPC" --field number)"
  if [[ "${LEXION_STRICT_BLOCK:-0}" == "1" ]] && (( captured != target )); then
    echo "error: requested block $target but captured $captured in strict mode" >&2
    exit 1
  fi
  if (( captured != target )); then
    echo "Trigger block $target captured canonical block $captured; using $captured for both routers."
    captured_audit="$OUTPUT_DIR/lexion-vs-uniswap-block-${captured}.json"
    mv "$audit" "$captured_audit"
    audit="$captured_audit"
  fi

  CAPTURED_AUDITS+=("$audit")
  CAPTURED_BLOCKS+=("$captured")
done

for index in "${!CAPTURED_AUDITS[@]}"; do
  audit="${CAPTURED_AUDITS[$index]}"
  target="${CAPTURED_BLOCKS[$index]}"
  echo "Comparing executable routes at captured block $target ..."
  anvil --fork-url "$UPSTREAM_RPC" --fork-block-number "$target" --port "$FORK_PORT" --silent \
    >"$ANVIL_LOG" 2>&1 &
  ANVIL_PID=$!
  for _ in $(seq 1 60); do
    cast block-number --rpc-url "$FORK_RPC" >/dev/null 2>&1 && break
    sleep 1
  done
  cast block-number --rpc-url "$FORK_RPC" >/dev/null || {
    echo "error: Anvil did not become healthy; see $ANVIL_LOG" >&2
    exit 1
  }

  UNISWAP_RPC_URL="$FORK_RPC" npx --yes node@20 \
    tools/uniswap-router-bench/normalized-audit.cjs "$audit"
  kill "$ANVIL_PID" 2>/dev/null || true
  wait "$ANVIL_PID" 2>/dev/null || true
  unset ANVIL_PID
done
