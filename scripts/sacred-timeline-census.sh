#!/usr/bin/env bash
# Runs the router with Sacred Timeline instrumentation and prints one aggregate summary.
#
# All arguments are forwarded to scripts/bench.sh. The verbose per-order census is retained in a
# temporary log whose path is printed at the end, so the normal output stays small while detailed
# diagnostics remain available.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG="$(mktemp -t fynd-sacred-timeline.XXXXXX.log)"

cd "$REPO_ROOT"

if ! FYND_SACRED_TIMELINE_CENSUS=1 \
  FYND_SACRED_TIMELINE_ALLOCATION_CENSUS=1 \
  ./scripts/bench.sh "$@" >"$LOG" 2>&1; then
  echo "Sacred Timeline benchmark failed. Last 80 log lines:" >&2
  tail -n 80 "$LOG" >&2
  echo "Full log: $LOG" >&2
  exit 1
fi

awk '
  /^=== SacredTimelineCensusV1 ===/ { frontier_reports++ }
  /^prefix states visited:/ { prefix_states += $NF }
  /^path extensions considered:/ { path_extensions += $NF }
  /^resource-conflict extensions:/ { resource_conflicts += $NF }
  /^complete portfolios classified:/ { portfolios += $NF }
  /^gross-bound certified dead:/ { gross_dead += $NF }
  /^V4 refinement attempts:/ { v4_attempts += $NF }
  /^V4 refinement successes:/ { v4_successes += $NF }
  /^V4 incumbent improvements:/ { v4_improvements += $NF }
  /^V4 losing frontier results:/ { v4_losers += $NF }
  /^V4 refinement time us:/ { v4_time_us += $NF }
  /^V4 bound-supported frontiers:/ { v4_bound_supported += $NF }
  /^V4 bound-unsupported frontiers:/ { v4_bound_unsupported += $NF }
  /^V4 frontiers pruned before refine:/ { v4_pre_pruned += $NF }
  /^V4 finite-input bound supported:/ { finite_supported += $NF }
  /^V4 finite-input bound would prune:/ { finite_would_prune += $NF }
  /^V4 finite-input refine time avoidable us:/ { finite_time_us += $NF }
  /^V4 finite-input certificate violations:/ { finite_violations += $NF }

  /^=== SacredTimelineAllocationCensusV1 ===/ { allocation_reports++ }
  /^intervals examined:/ { intervals += $NF }
  /^certified-bound pruned:/ { interval_pruned += $NF }
  /^singleton intervals replayed:/ { singleton_intervals += $NF }
  /^incumbent improvements:/ { interval_improvements += $NF }
  /^envelope simulator replays:/ { envelope_replays += $NF }
  /^exact-leaf simulator replays:/ { exact_replays += $NF }
  /^unsupported:/ { unsupported += ($NF == "true") }
  /^budget exceeded:/ { budget_exceeded += ($NF == "true") }
  /^branch-and-bound time us:/ { bnb_time_us += $NF }

  /^cutoff result / {
    threshold = ""
    for (field = 3; field <= NF; field++) {
      split($field, pair, "=")
      if (pair[1] == "centi_bps") threshold = pair[2]
      else if (pair[1] == "supported") cutoff_supported[threshold] += pair[2]
      else if (pair[1] == "pruned") cutoff_pruned[threshold] += pair[2]
      else if (pair[1] == "refinements_avoided") cutoff_avoided[threshold] += pair[2]
      else if (pair[1] == "refinement_time_avoided_us") cutoff_time_avoided[threshold] += pair[2]
      else if (pair[1] == "improvements") cutoff_improvements[threshold] += pair[2]
      else if (pair[1] == "changed") cutoff_changed[threshold] += (pair[2] == "true")
      else if (pair[1] == "loss_bps") {
        cutoff_loss_sum[threshold] += pair[2]
        if (pair[2] > cutoff_loss_max[threshold]) cutoff_loss_max[threshold] = pair[2]
      }
    }
  }

  /^allocation cutoff result / {
    threshold = ""
    for (field = 4; field <= NF; field++) {
      split($field, pair, "=")
      if (pair[1] == "centi_bps") threshold = pair[2]
      else if (pair[1] == "supported") allocation_cutoff_supported[threshold] += pair[2]
      else if (pair[1] == "pruned") allocation_cutoff_pruned[threshold] += pair[2]
      else if (pair[1] == "intervals_avoided") allocation_cutoff_intervals_avoided[threshold] += pair[2]
      else if (pair[1] == "envelope_replays_avoided") allocation_cutoff_envelope_avoided[threshold] += pair[2]
      else if (pair[1] == "leaf_replays_avoided") allocation_cutoff_leaf_avoided[threshold] += pair[2]
      else if (pair[1] == "changed") allocation_cutoff_changed[threshold] += (pair[2] == "true")
      else if (pair[1] == "loss_bps") {
        allocation_cutoff_loss_sum[threshold] += pair[2]
        if (pair[2] > allocation_cutoff_loss_max[threshold]) allocation_cutoff_loss_max[threshold] = pair[2]
      }
    }
  }

  END {
    classified = path_extensions + portfolios
    dead = resource_conflicts + gross_dead
    dead_pct = classified ? 100 * dead / classified : 0
    losing_total = v4_improvements + v4_losers
    losing_pct = losing_total ? 100 * v4_losers / losing_total : 0
    supported_prune_pct = v4_bound_supported ? 100 * v4_pre_pruned / v4_bound_supported : 0
    interval_prune_pct = intervals ? 100 * interval_pruned / intervals : 0

    print ""
    print "=== Sacred Timeline aggregate ==="
    printf "orders/reports                   %d\n", frontier_reports
    printf "prefix states                    %d\n", prefix_states
    printf "path extensions                  %d\n", path_extensions
    printf "resource conflicts               %d\n", resource_conflicts
    printf "gross-bound dead portfolios      %d\n", gross_dead
    printf "classified dead                  %d / %d (%.2f%%)\n", dead, classified, dead_pct
    print ""
    printf "V4 bound supported               %d\n", v4_bound_supported
    printf "V4 bound unsupported             %d\n", v4_bound_unsupported
    printf "V4 pruned before refinement      %d (%.2f%% of supported)\n", v4_pre_pruned, supported_prune_pct
    printf "V4 refinement attempts           %d\n", v4_attempts
    printf "V4 refinement successes          %d\n", v4_successes
    printf "V4 incumbent improvements        %d\n", v4_improvements
    printf "V4 losing results                %d (%.2f%%)\n", v4_losers, losing_pct
    printf "V4 refinement time               %.3f ms\n", v4_time_us / 1000
    printf "finite-input bound supported      %d\n", finite_supported
    printf "finite-input would prune          %d\n", finite_would_prune
    printf "finite-input refine time avoidable %.3f ms\n", finite_time_us / 1000
    printf "finite-input violations           %d\n", finite_violations
    print ""
    printf "exact allocation reports         %d\n", allocation_reports
    printf "intervals examined               %d\n", intervals
    printf "intervals certificate-pruned     %d (%.2f%%)\n", interval_pruned, interval_prune_pct
    printf "singleton intervals replayed     %d\n", singleton_intervals
    printf "interval incumbent improvements  %d\n", interval_improvements
    printf "simulator replays                %d\n", envelope_replays + exact_replays
    printf "unsupported exact searches       %d\n", unsupported
    printf "budget-exceeded exact searches   %d\n", budget_exceeded
    printf "exact branch-and-bound time      %.3f ms\n", bnb_time_us / 1000
    print ""
    print "counterfactual cutoff sweep"
    print "threshold  supported  pruned  refinements avoided  refine ms avoided  changed orders  mean loss bps  max loss bps"
    split("0 1 10 50 100", cutoff_order, " ")
    for (cutoff_index = 1; cutoff_index <= 5; cutoff_index++) {
      threshold = cutoff_order[cutoff_index]
      mean_loss = frontier_reports ? cutoff_loss_sum[threshold] / frontier_reports : 0
      printf "%7.2f  %9d  %6d  %19d  %17.3f  %14d  %13.9f  %12.9f\n",
        threshold / 100,
        cutoff_supported[threshold],
        cutoff_pruned[threshold],
        cutoff_avoided[threshold],
        cutoff_time_avoided[threshold] / 1000,
        cutoff_changed[threshold],
        mean_loss,
        cutoff_loss_max[threshold]
    }
    print ""
    print "exact-allocation counterfactual cutoff sweep"
    print "threshold  bounds tested  bound-pruned  intervals avoided  simulator replays avoided  changed searches  mean loss bps  max loss bps"
    for (cutoff_index = 1; cutoff_index <= 5; cutoff_index++) {
      threshold = cutoff_order[cutoff_index]
      mean_loss = allocation_reports ? allocation_cutoff_loss_sum[threshold] / allocation_reports : 0
      printf "%7.2f  %13d  %12d  %17d  %25d  %16d  %13.9f  %12.9f\n",
        threshold / 100,
        allocation_cutoff_supported[threshold],
        allocation_cutoff_pruned[threshold],
        allocation_cutoff_intervals_avoided[threshold],
        allocation_cutoff_envelope_avoided[threshold] + allocation_cutoff_leaf_avoided[threshold],
        allocation_cutoff_changed[threshold],
        mean_loss,
        allocation_cutoff_loss_max[threshold]
    }
    print "=== end Sacred Timeline aggregate ==="
  }
' "$LOG"

REPORT="$(sed -n 's/^  wrote \(artifacts\/benchmarks\/runs\/.*\/report.md\)$/\1/p' "$LOG" | tail -n 1)"
if [[ -n "$REPORT" ]]; then
  echo "Benchmark report: $REPO_ROOT/$REPORT"
fi
echo "Detailed log: $LOG"
