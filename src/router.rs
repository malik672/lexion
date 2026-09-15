use smallvec::SmallVec;

use crate::{
    Amount, PoolId, TokenId,
    market::{MarketSnapshot, MarketTopology},
};

const INLINE_HOPS: usize = 4;
const INLINE_TOKENS: usize = INLINE_HOPS + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hop {
    pub pool: PoolId,
    pub token_in: TokenId,
    pub token_out: TokenId,
}

/// One structural route through the frozen market topology.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Path {
    pub hops: Box<[Hop]>,
}

/// Paths discovered once for one token pair and reusable across block snapshots.
#[derive(Clone, Debug)]
pub struct PathSet {
    pub token_in: TokenId,
    pub token_out: TokenId,
    pub paths: Box<[Path]>,
}

impl PathSet {
    pub fn discover(
        topology: &MarketTopology,
        token_in: TokenId,
        token_out: TokenId,
        max_hops: usize,
    ) -> Self {
        assert!(max_hops > 0, "max_hops must be positive");
        let mut paths = Vec::new();
        if token_in != token_out {
            let mut search = PathDiscovery {
                topology,
                target: token_out,
                max_hops,
                visited_tokens: SmallVec::from_slice(&[token_in]),
                used_pools: SmallVec::new(),
                hops: SmallVec::new(),
                paths: &mut paths,
            };
            search.visit(token_in);
        }
        Self {
            token_in,
            token_out,
            paths: paths.into_boxed_slice(),
        }
    }
}

struct PathDiscovery<'a> {
    topology: &'a MarketTopology,
    target: TokenId,
    max_hops: usize,
    visited_tokens: SmallVec<[TokenId; INLINE_TOKENS]>,
    used_pools: SmallVec<[PoolId; INLINE_HOPS]>,
    hops: SmallVec<[Hop; INLINE_HOPS]>,
    paths: &'a mut Vec<Path>,
}

impl PathDiscovery<'_> {
    fn visit(&mut self, current: TokenId) {
        if self.hops.len() == self.max_hops {
            return;
        }

        for (pool, next) in self.topology.adjacent(current) {
            if self.used_pools.contains(&pool) || self.visited_tokens.contains(&next) {
                continue;
            }

            self.used_pools.push(pool);
            self.visited_tokens.push(next);
            self.hops.push(Hop {
                pool,
                token_in: current,
                token_out: next,
            });

            if next == self.target {
                self.paths.push(Path {
                    hops: self.hops.clone().into_vec().into_boxed_slice(),
                });
            } else {
                self.visit(next);
            }

            self.hops.pop();
            self.visited_tokens.pop();
            self.used_pools.pop();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Quote {
    pub block_number: u64,
    pub amount_in: Amount,
    pub amount_out: Amount,
    pub hops: Box<[Hop]>,
}

pub struct Router<'market> {
    topology: &'market MarketTopology,
    snapshot: &'market MarketSnapshot,
}

impl<'market> Router<'market> {
    pub fn new(topology: &'market MarketTopology, snapshot: &'market MarketSnapshot) -> Self {
        Self { topology, snapshot }
    }

    pub fn quote_paths(&self, paths: &PathSet, amount_in: Amount) -> Option<Quote> {
        paths
            .paths
            .iter()
            .filter_map(|path| {
                self.replay_path(path, amount_in).map(|amount_out| Quote {
                    block_number: self.snapshot.block_number(),
                    amount_in,
                    amount_out,
                    hops: path.hops.clone(),
                })
            })
            .max_by_key(|quote| quote.amount_out)
    }

    pub fn replay_path(&self, path: &Path, amount_in: Amount) -> Option<Amount> {
        self.replay_path_with_gas(path, amount_in)
            .map(|(amount, _)| amount)
    }

    pub fn replay_path_with_gas(&self, path: &Path, amount_in: Amount) -> Option<(Amount, Amount)> {
        let mut amount = amount_in;
        let mut gas = Amount::ZERO;
        for hop in &path.hops {
            let connection = self.topology.connection(hop.pool);
            let quote = self.snapshot.pool(hop.pool).quote_exact_input_with_gas(
                hop.token_in,
                connection.token_a,
                connection.token_b,
                amount,
            )?;
            amount = quote.0;
            gas = gas.checked_add(quote.1)?;
        }
        Some((amount, gas))
    }
}

#[cfg(test)]
mod tests {
    use super::{PathSet, Router};
    use crate::{
        Amount, TokenId,
        market::MarketBuilder,
        simulator::{PoolState, V2State},
    };

    fn example_market() -> (
        crate::market::MarketTopology,
        crate::market::MarketSnapshot,
        [crate::PoolId; 3],
    ) {
        let a = TokenId(0);
        let b = TokenId(1);
        let c = TokenId(2);
        let mut market = MarketBuilder::default();
        let direct = market.add_v2_pool(a, c, Amount::from(1_000_000), Amount::from(900_000), 30);
        let first = market.add_v2_pool(a, b, Amount::from(1_000_000), Amount::from(1_000_000), 30);
        let second = market.add_v2_pool(b, c, Amount::from(1_000_000), Amount::from(1_000_000), 30);
        let (topology, snapshot) = market.finish(100);
        (topology, snapshot, [direct, first, second])
    }

    #[test]
    fn discovers_once_and_selects_the_best_replayed_path() {
        let (topology, snapshot, [direct, first, second]) = example_market();
        let paths = PathSet::discover(&topology, TokenId(0), TokenId(2), 2);
        assert_eq!(paths.paths.len(), 2);

        let quote = Router::new(&topology, &snapshot)
            .quote_paths(&paths, Amount::from(10_000))
            .unwrap();
        assert_eq!(
            quote.hops.iter().map(|hop| hop.pool).collect::<Vec<_>>(),
            [first, second]
        );
        assert_ne!(quote.hops[0].pool, direct);
        assert_eq!(quote.amount_out, Amount::from(9_745));
        assert_eq!(quote.block_number, 100);
    }

    #[test]
    fn respects_the_hop_limit() {
        let (topology, _, _) = example_market();
        assert_eq!(
            PathSet::discover(&topology, TokenId(0), TokenId(2), 1)
                .paths
                .len(),
            1
        );
        assert_eq!(
            PathSet::discover(&topology, TokenId(0), TokenId(2), 2)
                .paths
                .len(),
            2
        );
    }

    #[test]
    fn rejects_token_cycles() {
        let a = TokenId(0);
        let b = TokenId(1);
        let c = TokenId(2);
        let mut market = MarketBuilder::default();
        market.add_v2_pool(a, b, Amount::from(100), Amount::from(100), 30);
        market.add_v2_pool(b, a, Amount::from(100), Amount::from(100), 30);
        market.add_v2_pool(b, c, Amount::from(100), Amount::from(100), 30);
        let (topology, _) = market.finish(1);

        let paths = PathSet::discover(&topology, a, c, 3);
        assert_eq!(paths.paths.len(), 2);
        assert!(paths.paths.iter().all(|path| path.hops.len() == 2));
    }

    #[test]
    fn replays_one_path_set_across_snapshots() {
        let a = TokenId(0);
        let b = TokenId(1);
        let mut builder = MarketBuilder::default();
        let pool = builder.add_v2_pool(a, b, Amount::from(1_000_000), Amount::from(1_000_000), 30);
        let (topology, block_100) = builder.finish(100);
        let paths = PathSet::discover(&topology, a, b, 1);
        let block_101 = topology.next_snapshot(
            &block_100,
            101,
            [(
                pool,
                PoolState::V2(V2State::new(
                    Amount::from(2_000_000),
                    Amount::from(1_000_000),
                    30,
                )),
            )],
        );

        let old = Router::new(&topology, &block_100)
            .quote_paths(&paths, Amount::from(10_000))
            .unwrap();
        let new = Router::new(&topology, &block_101)
            .quote_paths(&paths, Amount::from(10_000))
            .unwrap();
        assert_eq!(old.block_number, 100);
        assert_eq!(new.block_number, 101);
        assert!(new.amount_out < old.amount_out);
    }
}
