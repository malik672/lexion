#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ORIGINAL_ROOT="${LEXION_ORIGINAL_ROOT:-$ROOT/../fynd-hybrid-router}"
PORT="${LEXION_PARITY_PORT:-30320}"
URL="http://127.0.0.1:$PORT"
LOG="$ROOT/target/parity-original-server.log"

if [[ -f "$ORIGINAL_ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ORIGINAL_ROOT/.env"
  set +a
fi

: "${TYCHO_API_KEY:?TYCHO_API_KEY is required}"
: "${TYCHO_URL:?TYCHO_URL is required}"
: "${RPC_URL:?RPC_URL is required}"
export TYCHO_HOST="${TYCHO_HOST:-$TYCHO_URL}"

mkdir -p "$ROOT/target"
cd "$ORIGINAL_ROOT"
cargo run --release --no-default-features -p fynd -- serve \
  --http-host 127.0.0.1 \
  --http-port "$PORT" \
  --tycho-url "$TYCHO_URL" \
  --tycho-api-key "$TYCHO_API_KEY" \
  --rpc-url "$RPC_URL" \
  --protocols uniswap_v2,uniswap_v3,uniswap_v4 \
  --min-tvl 10 \
  --worker-pools-config "$ROOT/scripts/parity-worker-pools.toml" >"$LOG" 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true' EXIT

for _ in $(seq 1 180); do
  if curl -sf "$URL/v1/health" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    tail -n 80 "$LOG" >&2
    exit 1
  fi
  sleep 1
done
curl -sf "$URL/v1/health" >/dev/null || {
  tail -n 80 "$LOG" >&2
  exit 1
}

cd "$ROOT"
cargo run --release --features tycho --bin parity-original -- \
  --original-url "$URL" \
  --max-hops "${LEXION_MAX_HOPS:-2}" \
  --min-tvl "${LEXION_MIN_TVL:-10}" \
  "$@"
