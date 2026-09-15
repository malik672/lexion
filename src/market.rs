use crate::{
    Amount, PoolId, TokenId,
    simulator::{PoolKind, PoolState, V2State},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use arc_swap::ArcSwap;

#[cfg(feature = "tycho")]
use crate::tycho::TychoPool;

#[derive(Clone, Copy, Debug)]
pub struct PoolConnection {
    pub token_a: TokenId,
    pub token_b: TokenId,
    pub kind: PoolKind,
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    pool: PoolId,
    token_out: TokenId,
}

/// The frozen graph skeleton shared by every block snapshot.
#[derive(Clone)]
pub struct MarketTopology {
    connections: Box<[PoolConnection]>,
    adjacency: Box<[Box<[Edge]>]>,
}

impl MarketTopology {
    pub fn pool_count(&self) -> usize {
        self.connections.len()
    }

    pub fn token_degree(&self, token: TokenId) -> usize {
        self.adjacency
            .get(token.0 as usize)
            .map_or(0, |edges| edges.len())
    }

    /// Counts adjacency entries whose reverse entry is absent.
    pub fn asymmetric_edge_count(&self) -> usize {
        self.adjacency
            .iter()
            .enumerate()
            .flat_map(|(token, edges)| {
                edges.iter().filter(move |edge| {
                    !self
                        .adjacency
                        .get(edge.token_out.0 as usize)
                        .is_some_and(|reverse| {
                            reverse.iter().any(|candidate| {
                                candidate.pool == edge.pool
                                    && candidate.token_out == TokenId(token as u32)
                            })
                        })
                })
            })
            .count()
    }

    pub fn connection(&self, id: PoolId) -> PoolConnection {
        self.connections[id.0 as usize]
    }

    pub fn adjacent(&self, token: TokenId) -> impl Iterator<Item = (PoolId, TokenId)> + '_ {
        self.adjacency
            .get(token.0 as usize)
            .into_iter()
            .flat_map(|edges| edges.iter())
            .map(|edge| (edge.pool, edge.token_out))
    }

    /// Creates the next immutable snapshot while reusing this topology.
    pub fn next_snapshot(
        &self,
        previous: &MarketSnapshot,
        block_number: u64,
        updates: impl IntoIterator<Item = (PoolId, PoolState)>,
    ) -> MarketSnapshot {
        assert!(
            block_number > previous.block_number,
            "blocks must move forward"
        );
        assert_eq!(previous.pools.len(), self.connections.len());
        let mut pools = previous.pools.to_vec();
        for (pool, state) in updates {
            let index = pool.0 as usize;
            assert_eq!(self.connections[index].kind, state.kind());
            pools[index] = Arc::new(state);
        }
        MarketSnapshot {
            block_number,
            pools: pools.into_boxed_slice(),
        }
    }
}

/// All mutable pool data captured at one exact block.
#[derive(Clone)]
pub struct MarketSnapshot {
    block_number: u64,
    pools: Box<[Arc<PoolState>]>,
}

impl MarketSnapshot {
    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    pub fn pool(&self, id: PoolId) -> &PoolState {
        &self.pools[id.0 as usize]
    }
}

/// Lock-free publication point for complete immutable market snapshots.
pub struct PublishedMarket {
    current: ArcSwap<MarketView>,
    healthy: AtomicBool,
}

/// One internally consistent topology and state generation.
pub struct MarketView {
    topology: Arc<MarketTopology>,
    snapshot: Arc<MarketSnapshot>,
    topology_generation: u64,
}

impl MarketView {
    pub fn topology(&self) -> &MarketTopology {
        &self.topology
    }

    pub fn snapshot(&self) -> &MarketSnapshot {
        &self.snapshot
    }

    pub fn topology_generation(&self) -> u64 {
        self.topology_generation
    }
}

impl PublishedMarket {
    pub fn new(topology: MarketTopology, snapshot: MarketSnapshot) -> Self {
        Self {
            current: ArcSwap::from_pointee(MarketView {
                topology: Arc::new(topology),
                snapshot: Arc::new(snapshot),
                topology_generation: 0,
            }),
            healthy: AtomicBool::new(true),
        }
    }

    /// Pins a mutually consistent topology and snapshot for one quote.
    pub fn view(&self) -> Arc<MarketView> {
        self.current.load_full()
    }

    pub fn topology(&self) -> Arc<MarketTopology> {
        Arc::clone(&self.current.load().topology)
    }

    /// Pins one complete block snapshot for the lifetime of a quote.
    pub fn snapshot(&self) -> Arc<MarketSnapshot> {
        Arc::clone(&self.current.load().snapshot)
    }

    /// Publishes a new state snapshot while retaining the existing topology generation.
    pub fn publish_snapshot(&self, snapshot: MarketSnapshot) {
        let current = self.current.load();
        self.current.store(Arc::new(MarketView {
            topology: Arc::clone(&current.topology),
            snapshot: Arc::new(snapshot),
            topology_generation: current.topology_generation,
        }));
    }

    /// Publishes a rebuilt topology and matching snapshot as the next generation.
    pub fn publish_topology(&self, topology: MarketTopology, snapshot: MarketSnapshot) {
        let generation = self.current.load().topology_generation + 1;
        self.current.store(Arc::new(MarketView {
            topology: Arc::new(topology),
            snapshot: Arc::new(snapshot),
            topology_generation: generation,
        }));
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub fn set_healthy(&self, healthy: bool) {
        self.healthy.store(healthy, Ordering::Release);
    }
}

/// Construction-only market. Calling `finish` freezes its topology and first snapshot.
#[derive(Default)]
pub struct MarketBuilder {
    connections: Vec<PoolConnection>,
    adjacency: Vec<Vec<Edge>>,
    pools: Vec<Arc<PoolState>>,
}

impl MarketBuilder {
    pub fn add_v2_pool(
        &mut self,
        token_a: TokenId,
        token_b: TokenId,
        reserve_a: Amount,
        reserve_b: Amount,
        fee_bps: u16,
    ) -> PoolId {
        assert!(token_a != token_b, "a pool needs two different tokens");
        let state = PoolState::V2(V2State::new(reserve_a, reserve_b, fee_bps));
        let id = PoolId(self.connections.len().try_into().expect("too many pools"));
        let token_count = token_a.0.max(token_b.0) as usize + 1;
        if self.adjacency.len() < token_count {
            self.adjacency.resize_with(token_count, Vec::new);
        }
        self.adjacency[token_a.0 as usize].push(Edge {
            pool: id,
            token_out: token_b,
        });
        self.adjacency[token_b.0 as usize].push(Edge {
            pool: id,
            token_out: token_a,
        });
        self.connections.push(PoolConnection {
            token_a,
            token_b,
            kind: state.kind(),
        });
        self.pools.push(Arc::new(state));
        id
    }

    #[cfg(feature = "tycho")]
    pub fn add_tycho_pool(&mut self, pool: TychoPool) -> PoolId {
        let token_a = pool.token_a_id();
        let token_b = pool.token_b_id();
        assert!(token_a != token_b, "a pool needs two different tokens");
        let state = PoolState::Tycho(Box::new(pool));
        let id = PoolId(self.connections.len().try_into().expect("too many pools"));
        let token_count = token_a.0.max(token_b.0) as usize + 1;
        if self.adjacency.len() < token_count {
            self.adjacency.resize_with(token_count, Vec::new);
        }
        self.adjacency[token_a.0 as usize].push(Edge {
            pool: id,
            token_out: token_b,
        });
        self.adjacency[token_b.0 as usize].push(Edge {
            pool: id,
            token_out: token_a,
        });
        self.connections.push(PoolConnection {
            token_a,
            token_b,
            kind: state.kind(),
        });
        self.pools.push(Arc::new(state));
        id
    }

    pub fn finish(self, block_number: u64) -> (MarketTopology, MarketSnapshot) {
        let topology = MarketTopology {
            connections: self.connections.into_boxed_slice(),
            adjacency: self
                .adjacency
                .into_iter()
                .map(Vec::into_boxed_slice)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        };
        let snapshot = MarketSnapshot {
            block_number,
            pools: self.pools.into_boxed_slice(),
        };
        (topology, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{MarketBuilder, PublishedMarket};
    use crate::{Amount, TokenId, simulator::PoolState};

    #[test]
    fn publication_never_mutates_a_pinned_snapshot() {
        let mut builder = MarketBuilder::default();
        let pool = builder.add_v2_pool(
            TokenId(0),
            TokenId(1),
            Amount::from(100_u64),
            Amount::from(100_u64),
            30,
        );
        let (topology, first) = builder.finish(10);
        let second = topology.next_snapshot(
            &first,
            11,
            [(
                pool,
                PoolState::V2(crate::simulator::V2State::new(
                    Amount::from(200_u64),
                    Amount::from(100_u64),
                    30,
                )),
            )],
        );
        assert!(!std::sync::Arc::ptr_eq(&first.pools[0], &second.pools[0]));
        let published = PublishedMarket::new(topology, first);
        let pinned = published.snapshot();

        published.publish_snapshot(second);

        assert_eq!(pinned.block_number(), 10);
        assert_eq!(published.snapshot().block_number(), 11);
    }

    #[test]
    fn unchanged_pool_slots_are_shared_between_blocks() {
        let mut builder = MarketBuilder::default();
        let changed = builder.add_v2_pool(
            TokenId(0),
            TokenId(1),
            Amount::from(100_u64),
            Amount::from(100_u64),
            30,
        );
        builder.add_v2_pool(
            TokenId(1),
            TokenId(2),
            Amount::from(100_u64),
            Amount::from(100_u64),
            30,
        );
        let (topology, first) = builder.finish(10);
        let second = topology.next_snapshot(
            &first,
            11,
            [(
                changed,
                PoolState::V2(crate::simulator::V2State::new(
                    Amount::from(200_u64),
                    Amount::from(100_u64),
                    30,
                )),
            )],
        );

        assert!(!Arc::ptr_eq(&first.pools[0], &second.pools[0]));
        assert!(Arc::ptr_eq(&first.pools[1], &second.pools[1]));
    }

    #[test]
    fn adding_lower_token_ids_does_not_truncate_existing_adjacency() {
        let mut builder = MarketBuilder::default();
        builder.add_v2_pool(
            TokenId(10),
            TokenId(11),
            Amount::from(100_u64),
            Amount::from(100_u64),
            30,
        );
        builder.add_v2_pool(
            TokenId(0),
            TokenId(1),
            Amount::from(100_u64),
            Amount::from(100_u64),
            30,
        );
        let (topology, _) = builder.finish(1);

        assert_eq!(topology.token_degree(TokenId(10)), 1);
        assert_eq!(topology.token_degree(TokenId(11)), 1);
        assert_eq!(topology.asymmetric_edge_count(), 0);
    }

    #[test]
    fn topology_and_snapshot_publish_as_one_generation() {
        let mut first_builder = MarketBuilder::default();
        first_builder.add_v2_pool(
            TokenId(0),
            TokenId(1),
            Amount::from(100_u64),
            Amount::from(100_u64),
            30,
        );
        let (first_topology, first_snapshot) = first_builder.finish(10);
        let published = PublishedMarket::new(first_topology, first_snapshot);
        let old = published.view();

        let mut second_builder = MarketBuilder::default();
        second_builder.add_v2_pool(
            TokenId(1),
            TokenId(2),
            Amount::from(200_u64),
            Amount::from(200_u64),
            30,
        );
        let (second_topology, second_snapshot) = second_builder.finish(11);
        published.publish_topology(second_topology, second_snapshot);
        let new = published.view();

        assert_eq!(old.snapshot().block_number(), 10);
        assert_eq!(
            old.topology().connection(crate::PoolId(0)).token_a,
            TokenId(0)
        );
        assert_eq!(new.snapshot().block_number(), 11);
        assert_eq!(old.topology_generation(), 0);
        assert_eq!(new.topology_generation(), 1);
        assert_eq!(
            new.topology().connection(crate::PoolId(0)).token_a,
            TokenId(1)
        );
    }
}
