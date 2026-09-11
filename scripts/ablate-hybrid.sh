#!/usr/bin/env bash
set -euo pipefail

# Cumulative ablation. Every rung uses the same market, orders, depth, timeout, and worker count:
# native -> local exact refinement -> exact portfolio search -> multi-scale discovery
# -> V4 resource quotienting.
exec "$(dirname "$0")/bench.sh" \
  --configs path_frank_wolfe_native_d2,path_frank_wolfe_exact_refinement_d2,path_frank_wolfe_no_v4_exact_d2,path_frank_wolfe_multiscale_no_v4_d2,path_frank_wolfe_multiscale_v4_exhaustive_d2,path_frank_wolfe_multiscale_d2 \
  "$@"
