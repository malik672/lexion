//! Lock-free market reads and per-worker routing caches.

use std::{collections::HashMap, sync::Arc, time::Duration, time::Instant};

use crate::{
    Amount, TokenId,
    market::PublishedMarket,
    router::PathSet,
    solver::{Solution, Solver, SolverConfig},
};

#[derive(Clone, Copy, Debug)]
pub struct QuoteRuntimeConfig {
    pub max_hops: usize,
    pub max_block_lag: u64,
    pub solver: SolverConfig,
}

impl Default for QuoteRuntimeConfig {
    fn default() -> Self {
        Self {
            max_hops: 2,
            max_block_lag: 2,
            solver: SolverConfig::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct QuoteRequest {
    pub token_in: TokenId,
    pub token_out: TokenId,
    pub amount_in: Amount,
    /// The caller's current chain head, used only for the stale-market gate.
    pub chain_head: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct QuoteTimings {
    pub snapshot_pin: Duration,
    pub path_cache: Duration,
    pub path_discovery: Duration,
    pub solve: Duration,
    pub total: Duration,
}

#[derive(Clone, Debug)]
pub struct LiveQuote {
    pub solution: Solution,
    pub topology_generation: u64,
    pub path_count: usize,
    pub path_cache_hit: bool,
    pub timings: QuoteTimings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuoteError {
    FeedUnhealthy,
    StaleMarket { market_block: u64, chain_head: u64 },
    NoStructuralPath,
    AllPathsFailedSimulation,
}

/// One quote worker. Give each service worker its own instance to avoid cache locks.
pub struct QuoteWorker {
    market: Arc<PublishedMarket>,
    config: QuoteRuntimeConfig,
    cached_generation: Option<u64>,
    paths: HashMap<(TokenId, TokenId, usize), Arc<PathSet>>,
}

impl QuoteWorker {
    pub fn new(market: Arc<PublishedMarket>, config: QuoteRuntimeConfig) -> Self {
        assert!(config.max_hops > 0, "max_hops must be positive");
        Self {
            market,
            config,
            cached_generation: None,
            paths: HashMap::new(),
        }
    }

    pub fn quote(&mut self, request: QuoteRequest) -> Result<LiveQuote, QuoteError> {
        let started = Instant::now();
        if !self.market.is_healthy() {
            return Err(QuoteError::FeedUnhealthy);
        }

        let phase = Instant::now();
        let view = self.market.view();
        let snapshot_pin = phase.elapsed();
        let market_block = view.snapshot().block_number();
        if request.chain_head.saturating_sub(market_block) > self.config.max_block_lag {
            return Err(QuoteError::StaleMarket {
                market_block,
                chain_head: request.chain_head,
            });
        }

        let generation = view.topology_generation();
        if self.cached_generation != Some(generation) {
            self.paths.clear();
            self.cached_generation = Some(generation);
        }
        let key = (request.token_in, request.token_out, self.config.max_hops);
        let phase = Instant::now();
        let cached = self.paths.get(&key).cloned();
        let path_cache = phase.elapsed();
        let path_cache_hit = cached.is_some();
        let phase = Instant::now();
        let paths = cached.unwrap_or_else(|| {
            let paths = Arc::new(PathSet::discover(
                view.topology(),
                request.token_in,
                request.token_out,
                self.config.max_hops,
            ));
            self.paths.insert(key, Arc::clone(&paths));
            paths
        });
        let path_discovery = if path_cache_hit {
            Duration::ZERO
        } else {
            phase.elapsed()
        };
        if paths.paths.is_empty() {
            return Err(QuoteError::NoStructuralPath);
        }

        let phase = Instant::now();
        let solution = Solver::new(view.topology(), view.snapshot(), self.config.solver)
            .solve(&paths, request.amount_in)
            .ok_or(QuoteError::AllPathsFailedSimulation)?;
        let solve = phase.elapsed();
        let path_count = paths.paths.len();

        Ok(LiveQuote {
            solution,
            topology_generation: generation,
            path_count,
            path_cache_hit,
            timings: QuoteTimings {
                snapshot_pin,
                path_cache,
                path_discovery,
                solve,
                total: started.elapsed(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{QuoteError, QuoteRequest, QuoteRuntimeConfig, QuoteWorker};
    use crate::{Amount, TokenId, market::MarketBuilder, market::PublishedMarket};

    #[test]
    fn cache_survives_state_updates_and_invalidates_on_topology_updates() {
        let a = TokenId(0);
        let b = TokenId(1);
        let mut builder = MarketBuilder::default();
        let pool = builder.add_v2_pool(
            a,
            b,
            Amount::from(1_000_000_u64),
            Amount::from(1_000_000_u64),
            30,
        );
        let (topology, snapshot) = builder.finish(10);
        let market = Arc::new(PublishedMarket::new(topology.clone(), snapshot.clone()));
        let mut worker = QuoteWorker::new(Arc::clone(&market), QuoteRuntimeConfig::default());
        let request = QuoteRequest {
            token_in: a,
            token_out: b,
            amount_in: Amount::from(10_000_u64),
            chain_head: 10,
        };

        assert!(!worker.quote(request).unwrap().path_cache_hit);
        assert!(worker.quote(request).unwrap().path_cache_hit);

        let next = topology.next_snapshot(
            &snapshot,
            11,
            [(
                pool,
                crate::simulator::PoolState::V2(crate::simulator::V2State::new(
                    Amount::from(2_000_000_u64),
                    Amount::from(1_000_000_u64),
                    30,
                )),
            )],
        );
        market.publish_snapshot(next);
        let mut request = request;
        request.chain_head = 11;
        assert!(worker.quote(request).unwrap().path_cache_hit);

        market.publish_topology(topology, snapshot);
        assert!(!worker.quote(request).unwrap().path_cache_hit);
    }

    #[test]
    fn unhealthy_and_stale_markets_fail_closed() {
        let mut builder = MarketBuilder::default();
        builder.add_v2_pool(
            TokenId(0),
            TokenId(1),
            Amount::from(100_u64),
            Amount::from(100_u64),
            30,
        );
        let (topology, snapshot) = builder.finish(10);
        let market = Arc::new(PublishedMarket::new(topology, snapshot));
        let mut worker = QuoteWorker::new(Arc::clone(&market), QuoteRuntimeConfig::default());
        let request = QuoteRequest {
            token_in: TokenId(0),
            token_out: TokenId(1),
            amount_in: Amount::from(1_u64),
            chain_head: 13,
        };
        assert!(matches!(
            worker.quote(request),
            Err(QuoteError::StaleMarket {
                market_block: 10,
                chain_head: 13,
            })
        ));
        market.set_healthy(false);
        assert!(matches!(
            worker.quote(request),
            Err(QuoteError::FeedUnhealthy)
        ));
    }
}
