#!/usr/bin/env bash
set -euo pipefail

# Controlled A-E routing ablation on one captured market snapshot and order set:
# A ordered histories; B canonical subsets; C maximal frontiers;
# D maximal frontiers + exact zero-loss pruning; E D + 0.01-bps allocation cutoff.
exec "$(dirname "$0")/bench.sh" \
  --configs path_frank_wolfe_multiscale_v4_ordered_d2,path_frank_wolfe_multiscale_v4_exhaustive_unpruned_d2,path_frank_wolfe_multiscale_v4_maximal_unpruned_d2,path_frank_wolfe_multiscale_d2,path_frank_wolfe_multiscale_cutoff_001bps_d2 \
  "$@"
