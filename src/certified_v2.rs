//! Exact two-path V2 allocation.
//!
//! Ethereum-facing quantities remain `U256`. Unbounded integers are used only for rational curve
//! coefficients and interval certificates, which can grow beyond 256 bits during composition.

use num_bigint::BigUint;
use num_traits::{One, Zero};

use crate::{
    Amount,
    market::{MarketSnapshot, MarketTopology},
    router::{Path, Router},
};

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

    fn mul_uint(&self, value: &BigUint) -> Self {
        Self {
            num: &self.num * value,
            den: self.den.clone(),
        }
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
    fn one_hop(reserve_in: Amount, reserve_out: Amount, fee_bps: u16) -> Self {
        let fee_num = BigUint::from(10_000_u32 - u32::from(fee_bps));
        Self {
            a: to_big(reserve_out) * &fee_num,
            b: to_big(reserve_in) * BigUint::from(10_000_u32),
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

pub(crate) fn allocate_pair(
    topology: &MarketTopology,
    snapshot: &MarketSnapshot,
    left_path: &Path,
    right_path: &Path,
    total: Amount,
) -> Option<[(Amount, Amount); 2]> {
    if total.is_zero() {
        return None;
    }
    let left = curve(topology, snapshot, left_path)?;
    let right = curve(topology, snapshot, right_path)?;
    let router = Router::new(topology, snapshot);
    let total_big = to_big(total);

    let evaluate = |x: &BigUint| {
        let right_amount = from_big(x)?;
        let left_amount = total.checked_sub(right_amount)?;
        let left_output = if left_amount.is_zero() {
            Amount::ZERO
        } else {
            router.replay_path(left_path, left_amount)?
        };
        let right_output = if right_amount.is_zero() {
            Amount::ZERO
        } else {
            router.replay_path(right_path, right_amount)?
        };
        Some(left_output.checked_add(right_output)?)
    };

    let zero = BigUint::zero();
    let left_only = evaluate(&zero)?;
    let right_only = evaluate(&total_big)?;
    let (mut best_x, mut best) = if right_only >= left_only {
        (total_big.clone(), right_only)
    } else {
        (zero.clone(), left_only)
    };

    if let Some(root) = stationary_point(&left, &right, &total_big, &zero, &total_big) {
        for x in root_neighborhood(root, &total_big) {
            if let Some(value) = evaluate(&x)
                && value > best
            {
                best_x = x;
                best = value;
            }
        }
    }

    let mut intervals = vec![(zero, total_big.clone())];
    while let Some((lo, hi)) = intervals.pop() {
        let upper = interval_upper(&left, &right, &total_big, &lo, &hi)?;
        if upper <= to_big(best) {
            continue;
        }
        if lo == hi {
            if let Some(value) = evaluate(&lo)
                && value > best
            {
                best_x = lo;
                best = value;
            }
            continue;
        }
        let mid = (&lo + &hi) / BigUint::from(2_u8);
        intervals.push((&mid + BigUint::one(), hi));
        intervals.push((lo, mid));
    }

    let right_amount = from_big(&best_x)?;
    let left_amount = total.checked_sub(right_amount)?;
    let left_output = if left_amount.is_zero() {
        Amount::ZERO
    } else {
        router.replay_path(left_path, left_amount)?
    };
    let right_output = if right_amount.is_zero() {
        Amount::ZERO
    } else {
        router.replay_path(right_path, right_amount)?
    };
    Some([(left_amount, left_output), (right_amount, right_output)])
}

fn curve(topology: &MarketTopology, snapshot: &MarketSnapshot, path: &Path) -> Option<Curve> {
    let mut result = None;
    for hop in &path.hops {
        let connection = topology.connection(hop.pool);
        let state = snapshot.pool(hop.pool).as_v2()?;
        let (reserve_a, reserve_b) = state.reserves();
        let (reserve_in, reserve_out) = if hop.token_in == connection.token_a {
            (reserve_a, reserve_b)
        } else if hop.token_in == connection.token_b {
            (reserve_b, reserve_a)
        } else {
            return None;
        };
        let hop_curve = Curve::one_hop(reserve_in, reserve_out, state.fee_bps());
        result = Some(match result {
            Some(previous) => Curve::compose(&previous, &hop_curve),
            None => hop_curve,
        });
    }
    result
}

fn derivative_nonnegative(
    left: &Curve,
    right: &Curve,
    total: &BigUint,
    x: &BigUint,
) -> Option<bool> {
    let left_amount = total - x;
    let ld = left.derivative(&left_amount)?;
    let rd = right.derivative(x)?;
    Some(&rd.num * &ld.den >= &ld.num * &rd.den)
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
        let mid = (&low + &high) / BigUint::from(2_u8);
        if derivative_nonnegative(left, right, total, &mid)? {
            low = mid;
        } else {
            high = mid;
        }
    }
    Some(low)
}

fn interval_upper(
    left: &Curve,
    right: &Curve,
    total: &BigUint,
    lo: &BigUint,
    hi: &BigUint,
) -> Option<BigUint> {
    let mid = (lo + hi) / BigUint::from(2_u8);
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

fn root_neighborhood(root: BigUint, total: &BigUint) -> Vec<BigUint> {
    let mut points = vec![BigUint::zero(), total.clone(), root.clone()];
    for delta in 1..=2_u8 {
        let delta = BigUint::from(delta);
        if root >= delta {
            points.push(&root - &delta);
        }
        let above = &root + &delta;
        if above <= *total {
            points.push(above);
        }
    }
    points.sort_unstable();
    points.dedup();
    points
}

fn to_big(value: Amount) -> BigUint {
    BigUint::from_bytes_be(&value.to_be_bytes::<32>())
}

fn from_big(value: &BigUint) -> Option<Amount> {
    let bytes = value.to_bytes_be();
    (bytes.len() <= 32).then(|| Amount::from_be_slice(&bytes))
}

#[cfg(test)]
mod tests {
    use super::allocate_pair;
    use crate::{
        Amount, TokenId,
        market::MarketBuilder,
        router::{PathSet, Router},
    };

    #[test]
    fn certified_pair_matches_brute_force_integer_optimum() {
        let a = TokenId(0);
        let b = TokenId(1);
        let mut builder = MarketBuilder::default();
        builder.add_v2_pool(a, b, Amount::from(10_000), Amount::from(12_000), 30);
        builder.add_v2_pool(a, b, Amount::from(15_000), Amount::from(13_000), 25);
        let (topology, snapshot) = builder.finish(1);
        let paths = PathSet::discover(&topology, a, b, 1);
        let total = Amount::from(1_000);
        let pair = allocate_pair(
            &topology,
            &snapshot,
            &paths.paths[0],
            &paths.paths[1],
            total,
        )
        .unwrap();
        let certified = pair[0].1 + pair[1].1;

        let router = Router::new(&topology, &snapshot);
        let brute = (0..=1_000_u64)
            .map(|right| {
                let right = Amount::from(right);
                let left = total - right;
                let left_out = if left.is_zero() {
                    Amount::ZERO
                } else {
                    router.replay_path(&paths.paths[0], left).unwrap()
                };
                let right_out = if right.is_zero() {
                    Amount::ZERO
                } else {
                    router.replay_path(&paths.paths[1], right).unwrap()
                };
                left_out + right_out
            })
            .max()
            .unwrap();

        assert_eq!(certified, brute);
    }
}
