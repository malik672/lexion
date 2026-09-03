#!/usr/bin/env python3
"""Falsification harness for V3 amount-search ideas.

This is deliberately a synthetic model, not a Tycho benchmark. It generates
piecewise-concave path-output envelopes with downward marginal changes at
"tick" boundaries, floors the output to introduce integer execution noise,
and compares candidate pair solvers against exhaustive integer enumeration.

The purpose is methodological: reject search rules that are invalid even on a
structurally faithful toy domain before integrating them with UniswapV3State.
"""

from __future__ import annotations

import argparse
import math
import random
from dataclasses import dataclass


@dataclass(frozen=True)
class Segment:
    lo: int
    hi: int
    marginal_lo: float
    marginal_hi: float


def make_curve(rng: random.Random) -> list[Segment]:
    count = rng.randint(2, 5)
    x = 0
    marginal = rng.uniform(0.25, 6.0)
    segments: list[Segment] = []
    for _ in range(count):
        width = rng.randint(20, 140)
        marginal_end = max(1e-4, marginal - rng.uniform(0.0, 0.35 * marginal))
        segments.append(Segment(x, x + width, marginal, marginal_end))
        x += width
        # Tick crossing may change curvature/liquidity, but same-direction
        # execution must not jump to a better marginal price.
        marginal = marginal_end * rng.uniform(0.75, 1.0)
    return segments


def continuous_output(curve: list[Segment], amount: float) -> float:
    output = 0.0
    remaining = float(amount)
    for segment in curve:
        width = segment.hi - segment.lo
        taken = min(remaining, width)
        if taken <= 0:
            break
        output += (
            segment.marginal_lo * taken
            + 0.5
            * (segment.marginal_hi - segment.marginal_lo)
            * taken
            * taken
            / width
        )
        remaining -= taken
    if remaining > 0:
        output += remaining * curve[-1].marginal_hi
    return output


def replay_output(curve: list[Segment], amount: int) -> int:
    return math.floor(continuous_output(curve, amount))


def marginal(curve: list[Segment], amount: float) -> float:
    x = float(amount)
    for segment in curve:
        if x <= segment.hi:
            offset = max(0.0, x - segment.lo)
            return segment.marginal_lo + (
                segment.marginal_hi - segment.marginal_lo
            ) * offset / (segment.hi - segment.lo)
    return curve[-1].marginal_hi


def pair_output(left: list[Segment], right: list[Segment], total: int, x: int) -> int:
    return replay_output(left, x) + replay_output(right, total - x)


def exhaustive(left: list[Segment], right: list[Segment], total: int) -> tuple[int, int]:
    value, x = max((pair_output(left, right, total, x), x) for x in range(total + 1))
    return x, value


def golden_search(
    left: list[Segment], right: list[Segment], total: int, evaluations: int = 16
) -> tuple[int, int]:
    inv_phi = (math.sqrt(5.0) - 1.0) / 2.0
    lo = 0.0
    hi = float(total)

    def objective(x: float) -> int:
        xi = min(total, max(0, int(round(x))))
        return pair_output(left, right, total, xi)

    x1 = hi - inv_phi * (hi - lo)
    x2 = lo + inv_phi * (hi - lo)
    f1 = objective(x1)
    f2 = objective(x2)

    for _ in range(max(0, evaluations - 2)):
        if f1 < f2:
            lo = x1
            x1 = x2
            f1 = f2
            x2 = lo + inv_phi * (hi - lo)
            f2 = objective(x2)
        else:
            hi = x2
            x2 = x1
            f2 = f1
            x1 = hi - inv_phi * (hi - lo)
            f1 = objective(x1)

    center = int(round(x1 if f1 >= f2 else x2))
    candidates = range(max(0, center - 2), min(total, center + 2) + 1)
    value, x = max((pair_output(left, right, total, x), x) for x in candidates)
    return x, value


def marginal_root(left: list[Segment], right: list[Segment], total: int) -> float:
    lo = 0.0
    hi = float(total)
    for _ in range(48):
        x = (lo + hi) / 2.0
        difference = marginal(left, x) - marginal(right, total - x)
        if difference > 0:
            lo = x
        else:
            hi = x
    return (lo + hi) / 2.0


def marginal_with_replay_polish(
    left: list[Segment], right: list[Segment], total: int, radius: int = 32
) -> tuple[int, int]:
    center = int(round(marginal_root(left, right, total)))
    candidates = range(max(0, center - radius), min(total, center + radius) + 1)
    value, x = max((pair_output(left, right, total, x), x) for x in candidates)
    return x, value


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--trials", type=int, default=3000)
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--max-total", type=int, default=1000)
    parser.add_argument("--polish-radius", type=int, default=32)
    args = parser.parse_args()

    rng = random.Random(args.seed)
    golden_misses = 0
    marginal_misses = 0
    golden_max_gap = 0
    marginal_max_gap = 0

    for _ in range(args.trials):
        left = make_curve(rng)
        right = make_curve(rng)
        total = rng.randint(30, args.max_total)

        _, optimum = exhaustive(left, right, total)
        _, golden = golden_search(left, right, total)
        _, proposed = marginal_with_replay_polish(
            left, right, total, args.polish_radius
        )

        if golden != optimum:
            golden_misses += 1
            golden_max_gap = max(golden_max_gap, optimum - golden)
        if proposed != optimum:
            marginal_misses += 1
            marginal_max_gap = max(marginal_max_gap, optimum - proposed)

    print(f"trials={args.trials} seed={args.seed}")
    print(
        "golden: "
        f"misses={golden_misses} max_gap={golden_max_gap}"
    )
    print(
        "marginal+replay: "
        f"misses={marginal_misses} max_gap={marginal_max_gap} "
        f"radius={args.polish_radius}"
    )


if __name__ == "__main__":
    main()
