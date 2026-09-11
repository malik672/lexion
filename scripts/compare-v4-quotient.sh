#!/usr/bin/env bash
set -euo pipefail

# Same candidate discovery and allocator; only V4 portfolio enumeration differs.
exec "$(dirname "$0")/bench.sh" \
  --configs path_frank_wolfe_multiscale_v4_exhaustive_d2,path_frank_wolfe_multiscale_v4_certified_d2,path_frank_wolfe_multiscale_d2 \
  "$@"
