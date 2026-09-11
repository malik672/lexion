#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ -f "$REPO_ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$REPO_ROOT/.env"
  set +a
fi

RUN_DIR="${1:-artifacts/benchmarks/runs/v2-v3-live}"
TOOL_DIR="$REPO_ROOT/tools/uniswap-router-bench"
PUBLIC_ARCHIVE_RPC="https://ethereum-rpc.blockreq.com/v1/rpc/public"

if [[ ! -f "$RUN_DIR/uniswap-sor-comparison.json" ]]; then
  echo "error: $RUN_DIR/uniswap-sor-comparison.json is missing; run compare-uniswap-router.sh first" >&2
  exit 1
fi

CAPTURED_BLOCK="$(python3 - "$RUN_DIR/uniswap-sor-comparison.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))['block_number'])
PY
)"
CAPTURED_BLOCK_HEX="$(printf '0x%x' "$CAPTURED_BLOCK")"

rpc_works() {
  local url="$1"
  local body
  body="$(curl -sS --max-time 15 -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_call\",\"params\":[{\"to\":\"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2\",\"data\":\"0x313ce567\"},\"$CAPTURED_BLOCK_HEX\"]}" \
    "$url" 2>/dev/null || true)"
  [[ "$body" == *'"result":"0x'* ]]
}

RPC="${UNISWAP_RPC_URL:-$PUBLIC_ARCHIVE_RPC}"
echo "Checking forensic RPC at block $CAPTURED_BLOCK ..."
if ! rpc_works "$RPC"; then
  echo "error: RPC cannot serve historical eth_call at block $CAPTURED_BLOCK" >&2
  echo "set UNISWAP_RPC_URL in .env to a stable archive-capable Ethereum mainnet RPC" >&2
  exit 1
fi
export UNISWAP_RPC_URL="$RPC"

npm install --prefix "$TOOL_DIR" --no-audit --no-fund >/dev/null
npx --yes node@20 "$TOOL_DIR/inspect-counterexamples.cjs" "$RUN_DIR"
