use std::str::FromStr;

use alloy::primitives::U256;
use num_bigint::BigUint;
use tycho_simulation::{
    evm::protocol::{
        uniswap_v3::{enums::FeeAmount, state::UniswapV3State},
        utils::uniswap::tick_list::TickInfo,
    },
    tycho_common::{
        models::{token::Token, Chain},
        simulation::protocol_sim::ProtocolSim,
        Bytes,
    },
};

fn token(address: &str, symbol: &str) -> Token {
    Token::new(
        &Bytes::from_str(address).unwrap(),
        symbol,
        18,
        0,
        &[Some(10_000)],
        Chain::Ethereum,
        100,
    )
}

fn pool(liquidity: u128) -> UniswapV3State {
    // sqrt(2) * 2^96, approximately price=2. Current tick is near 6931.
    let sqrt_price = U256::from_str("112045541949572287496682733568").unwrap();
    UniswapV3State::new(
        liquidity,
        sqrt_price,
        FeeAmount::Low,
        6931,
        vec![
            TickInfo::new(-46080, 0).unwrap(),
            TickInfo::new(0, 0).unwrap(),
            TickInfo::new(46080, 0).unwrap(),
        ],
    )
    .unwrap()
}

fn quote(pool: &UniswapV3State, amount: u64, token_in: &Token, token_out: &Token) -> BigUint {
    pool.get_amount_out(BigUint::from(amount), token_in, token_out)
        .unwrap()
        .amount
}

fn terminal_marginal(
    pool: &UniswapV3State,
    amount: u64,
    token_in: &Token,
    token_out: &Token,
) -> f64 {
    let result = pool
        .get_amount_out(BigUint::from(amount), token_in, token_out)
        .unwrap();
    result.new_state.spot_price(token_in, token_out).unwrap()
}

fn exhaustive(
    left: &UniswapV3State,
    right: &UniswapV3State,
    total: u64,
    token_in: &Token,
    token_out: &Token,
) -> (u64, BigUint) {
    (0..=total)
        .map(|x| {
            let output = quote(left, x, token_in, token_out)
                + quote(right, total - x, token_in, token_out);
            (x, output)
        })
        .max_by(|(_, a), (_, b)| a.cmp(b))
        .unwrap()
}

fn marginal_search(
    left: &UniswapV3State,
    right: &UniswapV3State,
    total: u64,
    token_in: &Token,
    token_out: &Token,
    polish_radius: u64,
) -> (u64, BigUint) {
    let mut lo = 0u64;
    let mut hi = total;

    // Search the continuous KKT condition M_left(x) = M_right(total-x)
    // using the simulator's post-swap marginal price as the oracle.
    while hi.saturating_sub(lo) > 1 {
        let mid = lo + (hi - lo) / 2;
        let left_marginal = terminal_marginal(left, mid, token_in, token_out);
        let right_marginal = terminal_marginal(right, total - mid, token_in, token_out);
        if left_marginal > right_marginal {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    let center = lo + (hi - lo) / 2;
    let start = center.saturating_sub(polish_radius);
    let end = total.min(center.saturating_add(polish_radius));

    // Integer rounding is authoritative. Search a small exact neighborhood
    // around the continuous solution instead of assuming discrete concavity.
    (start..=end)
        .chain([0, total])
        .map(|x| {
            let output = quote(left, x, token_in, token_out)
                + quote(right, total - x, token_in, token_out);
            (x, output)
        })
        .max_by(|(_, a), (_, b)| a.cmp(b))
        .unwrap()
}

#[test]
fn marginal_search_matches_exhaustive_real_v3_state() {
    let token_in = token("0x0000000000000000000000000000000000000001", "X");
    let token_out = token("0x0000000000000000000000000000000000000002", "Y");

    // Unequal liquidity forces a non-50/50 optimum. The deliberately small
    // total makes full integer enumeration cheap enough to be an oracle.
    let left = pool(10_000);
    let right = pool(6_000);
    let total = 5_000u64;

    let exhaustive = exhaustive(&left, &right, total, &token_in, &token_out);
    let proposed = marginal_search(&left, &right, total, &token_in, &token_out, 32);

    assert_eq!(proposed.1, exhaustive.1, "proposed split must attain the exhaustive optimum");
}

#[test]
fn marginal_search_matches_exhaustive_across_liquidity_ratios() {
    let token_in = token("0x0000000000000000000000000000000000000001", "X");
    let token_out = token("0x0000000000000000000000000000000000000002", "Y");
    let total = 2_000u64;

    for right_liquidity in [2_500u128, 4_000, 6_000, 8_000, 10_000, 14_000] {
        let left = pool(10_000);
        let right = pool(right_liquidity);
        let exhaustive = exhaustive(&left, &right, total, &token_in, &token_out);
        let proposed = marginal_search(&left, &right, total, &token_in, &token_out, 32);
        assert_eq!(
            proposed.1, exhaustive.1,
            "failed for right liquidity {right_liquidity}; exhaustive x={}, proposed x={}",
            exhaustive.0, proposed.0
        );
    }
}
