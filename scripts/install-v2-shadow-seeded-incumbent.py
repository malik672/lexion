#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "fynd-core/src/algorithm/v2_certified_interval_allocator.rs"
text = TARGET.read_text()

old = '''    let incumbent = left_single.clone().max(right_single.clone());
    let mut best = incumbent.clone();
    let mut best_x = if right_single >= left_single { total.clone() } else { BigUint::zero() };

    let mut leaves = Vec::new();
    collect_survivors(
        &left,
        &right,
        total,
        &incumbent,
        BigUint::zero(),
        total.clone(),
        0,
        &mut stats,
        &mut leaves,
    );
'''

new = '''    let incumbent = left_single.clone().max(right_single.clone());
    let mut best = incumbent.clone();
    let mut best_x = if right_single >= left_single { total.clone() } else { BigUint::zero() };

    // Seed the proof search with a strong exact incumbent before subdividing.
    // The continuous objective is concave, so its derivative crossing localizes
    // the optimum. Exact replay remains authoritative: the continuous root and a
    // tiny integer neighborhood are only candidate generators.
    if let Some(root) = stationary_point(&left, &right, total, &BigUint::zero(), total) {
        let mut seed_points = Vec::new();
        push_unique(&mut seed_points, root.clone(), &BigUint::zero(), total);
        for delta in 1..=ROOT_NEIGHBORHOOD {
            let d = BigUint::from(delta);
            if root >= d {
                push_unique(&mut seed_points, &root - &d, &BigUint::zero(), total);
            }
            push_unique(&mut seed_points, &root + &d, &BigUint::zero(), total);
        }
        for x in seed_points {
            if let Some(value) = exact_output(current, total, &x, market, &mut stats.replays) {
                if value > best {
                    best = value;
                    best_x = x;
                }
            }
        }
    }
    let seed_best = best.clone();

    let mut leaves = Vec::new();
    collect_survivors(
        &left,
        &right,
        total,
        &best,
        BigUint::zero(),
        total.clone(),
        0,
        &mut stats,
        &mut leaves,
    );
'''

if new not in text:
    if old not in text:
        raise SystemExit("could not locate shadow incumbent block")
    text = text.replace(old, new, 1)

old_log = '''        "v2-certified-shadow: relation={} baseline={} shadow={} best_x={} baseline_x={} dead={}.{:02}% nodes={} dead_nodes={} leaves={} exact_replays={}",
        relation,
        baseline_out,
        best,
        best_x,
        baseline_x,
'''
new_log = '''        "v2-certified-shadow: relation={} baseline={} shadow={} seed_best={} best_x={} baseline_x={} dead={}.{:02}% nodes={} dead_nodes={} leaves={} exact_replays={}",
        relation,
        baseline_out,
        best,
        seed_best,
        best_x,
        baseline_x,
'''

if new_log not in text:
    if old_log not in text:
        raise SystemExit("could not locate shadow log block")
    text = text.replace(old_log, new_log, 1)

TARGET.write_text(text)
print("installed stationary-point seeded V2 shadow incumbent")
