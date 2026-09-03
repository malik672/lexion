//! Certified integer-rounding envelope for 1-2 hop Uniswap V2 paths.
//!
//! This module intentionally refuses paths longer than two hops. The bound below
//! is proved for the current bounded search language; extending it requires a
//! separate induction over suffix envelopes rather than extrapolating the
//! empirical recurrence.

use num_bigint::BigUint;

use super::{
    bellman_ford::BellmanFordContext,
    split_primitives::{simulate_path, HopDescriptor, MarketOverrides},
};
use tycho_simulation::evm::protocol::uniswap_v2::state::UniswapV2State;

fn is_v2_only(path: &[HopDescriptor], ctx: &BellmanFordContext) -> bool {
    !path.is_empty()
        && path.iter().all(|hop| {
            ctx.market_data
                .get_simulation_state(&hop.component_id)
                .is_some_and(|state| state.as_any().downcast_ref::<UniswapV2State>().is_some())
        })
}

/// Returns a proved upper bound on the gap between the ideal continuous V2
/// path response and exact integer replay, in final-output raw units.
///
/// For one hop, with continuous response
///
///     q(x) = a x / (b + c x),
///
/// exact replay is floor(q(x)), hence
///
///     0 <= q(x) - floor(q(x)) < 1.
///
/// For two hops, let q1 be the first continuous V2 response, q2 the second,
/// m = floor(q1(x)), and Q be exact integer replay. Then
///
///     q2(q1(x)) - Q(x)
///       = [q2(q1(x)) - q2(m)] + [q2(m) - floor(q2(m))].
///
/// Since 0 <= q1(x)-m < 1 and q2 is increasing and concave with q2(0)=0,
///
///     q2(q1(x)) - q2(m) <= q2(m+1)-q2(m) <= q2(1).
///
/// Also q2(m)-floor(q2(m)) < 1, so the total gap is strictly less than
/// q2(1)+1. Because floor(q2(1)) <= q2(1) < floor(q2(1))+1,
///
///     gap < floor(q2(1)) + 2.
///
/// The simulator's one-unit quote is exactly floor(q2(1)), giving the integer
/// certificate returned here.
///
/// `None` means the path is unsupported by this proof (non-V2, empty, >2 hops,
/// or the one-unit suffix quote itself cannot be simulated).
pub(super) fn certified_path_rounding_bound(
    path: &[HopDescriptor],
    ctx: &BellmanFordContext,
) -> Option<BigUint> {
    if !is_v2_only(path, ctx) {
        return None;
    }

    match path.len() {
        1 => Some(BigUint::from(1u8)),
        2 => {
            let suffix_one = simulate_path(
                &path[1..],
                &BigUint::from(1u8),
                &ctx.market_data,
                &MarketOverrides::empty(),
            )
            .ok()?
            .amount_out;
            Some(suffix_one + BigUint::from(2u8))
        }
        _ => None,
    }
}

/// Certified pair envelope for the exact split objective
///
///     F(x) = Q_left(T-x) + Q_right(x).
///
/// If `F*` is the corresponding ideal continuous V2 objective, then for all x
/// in [0,T],
///
///     F*(x) - F(x) < E_left + E_right.
///
/// The returned integer is therefore a conservative additive envelope that can
/// be combined with any sound upper bound on the continuous objective.
pub(super) fn certified_pair_rounding_bound(
    left: &[HopDescriptor],
    right: &[HopDescriptor],
    ctx: &BellmanFordContext,
) -> Option<BigUint> {
    Some(
        certified_path_rounding_bound(left, ctx)?
            + certified_path_rounding_bound(right, ctx)?,
    )
}
