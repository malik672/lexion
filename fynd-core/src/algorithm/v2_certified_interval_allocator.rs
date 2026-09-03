//! Shadow allocator for certified two-path Uniswap V2 refinement.
//!
//! This module never changes production routing. It uses the continuous no-floor
//! CPMM composition as a one-sided majorant, recursively discards allocation
//! intervals that cannot beat an exact singleton incumbent, and exact-replays a
//! small candidate set only inside surviving leaves. The result is compared with
//! the existing V2 allocator by the caller.

use num_bigint::BigUint;
use num_traits::{One, ToPrimitive, Zero};
use tycho_simulation::evm::protocol::uniswap_v2::state::UniswapV2State;

use super::super::{
    split_primitives::{simulate_path, HopDescriptor, MarketOverrides, PathAllocation},
    AlgorithmError,
};
use crate::feed::market_data::MarketState;

const SHADOW_DEPTH: usize = 6;
const ROOT_NEIGHBORHOOD: u64 = 2;

#[derive(Clone)]
struct Curve {
    a: BigUint,
    b: BigUint,
    c: BigUint,
}

#[derive(Clone)]
struct Fraction {
    num: BigUint,
    den: BigUint,
}

impl Fraction {
    fn new(num: BigUint, den: BigUint) -> Option<Self> {
        (!den.is_zero()).then_some(Self { num, den })
    }

    fn add(&self, other: &Self) -> Self {
        Self {
            num: &self.num * &other.den + &other.num * &self.den,
            den: &self.den * &other.den,
        }
    }

    fn mul_uint(&self, x: &BigUint) -> Self {
        Self { num: &self.num * x, den: self.den.clone() }
    }

    fn ceil(&self) -> BigUint {
        if self.num.is_zero() {
            BigUint::zero()
        } else {
            (&self.num + &self.den - BigUint::one()) / &self.den
        }
    }
}

impl Curve {
    fn one_hop(state: &UniswapV2State, zero_to_one: bool) -> Self {
        let (reserve_in, reserve_out) = if zero_to_one {
            (state.reserve0, state.reserve1)
        } else {
            (state.reserve1, state.reserve0)
        };
        let fee_num = BigUint::from(9_970u32);
        let fee_den = BigUint::from(10_000u32);
        Self {
            a: BigUint::from_bytes_be(&reserve_out.to_be_bytes::<32>()) * &fee_num,
            b: BigUint::from_bytes_be(&reserve_in.to_be_bytes::<32>()) * fee_den,
            c: fee_num,
        }
    }

    fn compose(first: &Self, second: &Self) -> Self {
        Self {
            a: &first.a * &second.a,
            b: &first.b * &second.b,
            c: &second.b * &first.c + &second.c * &first.a,
        }
    }

    fn value(&self, x: &BigUint) -> Option<Fraction> {
        Fraction::new(&self.a * x, &self.b + &self.c * x)
    }

    fn derivative(&self, x: &BigUint) -> Option<Fraction> {
        let base = &self.b + &self.c * x;
        Fraction::new(&self.a * &self.b, &base * &base)
    }
}

fn curve(path: &PathAllocation, market: &MarketState) -> Option<Curve> {
    let mut result = None;
    for hop in &path.hops {
        let d = &hop.descriptor;
        let state = market
            .get_simulation_state(&d.component_id)?
            .as_any()
            .downcast_ref::<UniswapV2State>()?;
        let hop_curve = Curve::one_hop(state, d.token_in.address < d.token_out.address);
        result = Some(match result {
            Some(previous) => Curve::compose(&previous, &hop_curve),
            None => hop_curve,
        });
    }
    result
}

fn descriptors(path: &PathAllocation) -> Vec<HopDescriptor> {
    path.hops.iter().map(|hop| hop.descriptor.clone()).collect()
}

fn exact_output(
    current: &[PathAllocation],
    total: &BigUint,
    x: &BigUint,
    market: &MarketState,
    replays: &mut usize,
) -> Option<BigUint> {
    if current.len() != 2 || x > total {
        return None;
    }
    let left_amount = total - x;
    let left_desc = descriptors(&current[0]);
    let right_desc = descriptors(&current[1]);
    let left = if left_amount.is_zero() {
        BigUint::zero()
    } else {
        *replays += 1;
        simulate_path(&left_desc, &left_amount, market, &MarketOverrides::empty()).ok()?.amount_out
    };
    let right = if x.is_zero() {
        BigUint::zero()
    } else {
        *replays += 1;
        simulate_path(&right_desc, x, market, &MarketOverrides::empty()).ok()?.amount_out
    };
    Some(left + right)
}

fn derivative_nonnegative(left: &Curve, right: &Curve, total: &BigUint, x: &BigUint) -> Option<bool> {
    let left_amount = total - x;
    let ld = left.derivative(&left_amount)?;
    let rd = right.derivative(x)?;
    Some(&rd.num * &ld.den >= &ld.num * &rd.den)
}

fn interval_upper(
    left: &Curve,
    right: &Curve,
    total: &BigUint,
    lo: &BigUint,
    hi: &BigUint,
) -> Option<BigUint> {
    if lo > hi || hi > total {
        return None;
    }
    let mid = (lo + hi) / BigUint::from(2u8);
    let left_amount = total - &mid;
    let value = left.value(&left_amount)?.add(&right.value(&mid)?);
    let ld = left.derivative(&left_amount)?;
    let rd = right.derivative(&mid)?;
    let nonnegative = &rd.num * &ld.den >= &ld.num * &rd.den;
    let slope = if nonnegative {
        Fraction::new(&rd.num * &ld.den - &ld.num * &rd.den, &rd.den * &ld.den)?
    } else {
        Fraction::new(&ld.num * &rd.den - &rd.num * &ld.den, &rd.den * &ld.den)?
    };
    let distance = if nonnegative { hi - &mid } else { &mid - lo };
    Some(value.add(&slope.mul_uint(&distance)).ceil())
}

#[derive(Default)]
struct Stats {
    nodes: usize,
    dead: usize,
    leaves: usize,
    dead_width: BigUint,
    replays: usize,
}

fn collect_survivors(
    left: &Curve,
    right: &Curve,
    total: &BigUint,
    incumbent: &BigUint,
    lo: BigUint,
    hi: BigUint,
    depth: usize,
    stats: &mut Stats,
    leaves: &mut Vec<(BigUint, BigUint)>,
) {
    stats.nodes += 1;
    let Some(upper) = interval_upper(left, right, total, &lo, &hi) else {
        stats.leaves += 1;
        leaves.push((lo, hi));
        return;
    };
    if upper <= *incumbent {
        stats.dead += 1;
        stats.dead_width += &hi - &lo;
        return;
    }
    if depth == SHADOW_DEPTH || lo == hi {
        stats.leaves += 1;
        leaves.push((lo, hi));
        return;
    }
    let mid = (&lo + &hi) / BigUint::from(2u8);
    if mid == lo || mid == hi {
        stats.leaves += 1;
        leaves.push((lo, hi));
        return;
    }
    collect_survivors(left, right, total, incumbent, lo.clone(), mid.clone(), depth + 1, stats, leaves);
    collect_survivors(left, right, total, incumbent, mid, hi, depth + 1, stats, leaves);
}

fn stationary_point(
    left: &Curve,
    right: &Curve,
    total: &BigUint,
    lo: &BigUint,
    hi: &BigUint,
) -> Option<BigUint> {
    if derivative_nonnegative(left, right, total, hi)? {
        return Some(hi.clone());
    }
    if !derivative_nonnegative(left, right, total, lo)? {
        return Some(lo.clone());
    }
    let mut low = lo.clone();
    let mut high = hi.clone();
    while &high - &low > BigUint::one() {
        let mid = (&low + &high) / BigUint::from(2u8);
        if derivative_nonnegative(left, right, total, &mid)? {
            low = mid;
        } else {
            high = mid;
        }
    }
    Some(low)
}

fn push_unique(points: &mut Vec<BigUint>, point: BigUint, lo: &BigUint, hi: &BigUint) {
    if point >= *lo && point <= *hi && !points.contains(&point) {
        points.push(point);
    }
}

fn leaf_points(left: &Curve, right: &Curve, total: &BigUint, lo: &BigUint, hi: &BigUint) -> Vec<BigUint> {
    let mut points = Vec::new();
    push_unique(&mut points, lo.clone(), lo, hi);
    push_unique(&mut points, hi.clone(), lo, hi);
    push_unique(&mut points, (lo + hi) / BigUint::from(2u8), lo, hi);
    if let Some(root) = stationary_point(left, right, total, lo, hi) {
        push_unique(&mut points, root.clone(), lo, hi);
        for delta in 1..=ROOT_NEIGHBORHOOD {
            let d = BigUint::from(delta);
            if root >= d {
                push_unique(&mut points, &root - &d, lo, hi);
            }
            push_unique(&mut points, &root + &d, lo, hi);
        }
    }
    points
}

pub(super) fn shadow_compare(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
    baseline: &[PathAllocation],
) -> Result<(), AlgorithmError> {
    if current.len() != 2 || total.is_zero() {
        return Ok(());
    }
    let (Some(left), Some(right)) = (curve(&current[0], market), curve(&current[1], market)) else {
        return Ok(());
    };

    let mut stats = Stats::default();
    let left_single = exact_output(current, total, &BigUint::zero(), market, &mut stats.replays);
    let right_single = exact_output(current, total, total, market, &mut stats.replays);
    let (Some(left_single), Some(right_single)) = (left_single, right_single) else {
        return Ok(());
    };
    let incumbent = left_single.clone().max(right_single.clone());
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

    for (lo, hi) in &leaves {
        for x in leaf_points(&left, &right, total, lo, hi) {
            if let Some(value) = exact_output(current, total, &x, market, &mut stats.replays) {
                if value > best {
                    best = value;
                    best_x = x;
                }
            }
        }
    }

    let baseline_out = baseline.iter().fold(BigUint::zero(), |sum, path| sum + &path.amount_out);
    let relation = if best < baseline_out { "LOSS" } else if best > baseline_out { "WIN" } else { "TIE" };
    let dead_bps = ((&stats.dead_width * BigUint::from(10_000u64)) / total)
        .to_u64()
        .unwrap_or(10_000)
        .min(10_000);
    let baseline_x = baseline
        .get(1)
        .map(|path| path.amount_in.clone())
        .unwrap_or_else(BigUint::zero);

    eprintln!(
        "v2-certified-shadow: relation={} baseline={} shadow={} best_x={} baseline_x={} dead={}.{:02}% nodes={} dead_nodes={} leaves={} exact_replays={}",
        relation,
        baseline_out,
        best,
        best_x,
        baseline_x,
        dead_bps / 100,
        dead_bps % 100,
        stats.nodes,
        stats.dead,
        stats.leaves,
        stats.replays,
    );
    Ok(())
}
