#!/usr/bin/env bash
# Compare a completed live Fynd hybrid benchmark against Uniswap's open-source Smart Order Router.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ -f "$REPO_ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$REPO_ROOT/.env"
  set +a
fi

RUN_DIR="${1:-bench-results/v2-v3-live}"
TOOL_DIR="$REPO_ROOT/tools/uniswap-router-bench"

if [[ -z "${RPC_URL:-}" ]]; then
  echo "error: RPC_URL is not set (put it in .env)" >&2
  exit 1
fi
if [[ ! -f "$RUN_DIR/orders.csv" || ! -f "$RUN_DIR/report.md" ]]; then
  echo "error: $RUN_DIR must contain orders.csv and report.md from a live benchmark" >&2
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  echo "error: npm is required" >&2
  exit 1
fi

if [[ ! -d "$TOOL_DIR/node_modules/@uniswap/smart-order-router" ]]; then
  echo "Installing pinned Uniswap Smart Order Router benchmark dependencies ..."
  npm install --prefix "$TOOL_DIR" --no-audit --no-fund
fi

echo "Running same-block Fynd vs Uniswap SOR comparison ..."
node "$TOOL_DIR/bench.cjs" "$RUN_DIR"
