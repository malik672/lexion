use crate::{
    Amount,
    allocator::allocate_by_pieces,
    frontier::{FrontierConfig, multiscale_frontier},
    market::{MarketSnapshot, MarketTopology},
    portfolio::maximal_portfolios,
    router::{PathSet, Router},
};

#[derive(Clone, Debug)]
pub struct Allocation {
    pub path_index: usize,
    pub amount_in: Amount,
    pub amount_out: Amount,
}

#[derive(Clone, Debug)]
pub struct Solution {
    pub block_number: u64,
    pub amount_in: Amount,
    pub amount_out: Amount,
    pub gas_estimate: Amount,
    pub net_amount_out: Amount,
    pub allocations: Box<[Allocation]>,
}

/// Converts gas units into output-token atomic units without rounding each gas unit separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GasValuation {
    pub cost_numerator: Amount,
    pub cost_denominator: Amount,
}

impl GasValuation {
    pub const ZERO: Self = Self {
        cost_numerator: Amount::ZERO,
        cost_denominator: Amount::from_limbs([1, 0, 0, 0]),
    };

    pub fn cost(self, gas: Amount) -> Amount {
        if gas.is_zero() || self.cost_numerator.is_zero() || self.cost_denominator.is_zero() {
            Amount::ZERO
        } else {
            gas.saturating_mul(self.cost_numerator)
                .div_ceil(self.cost_denominator)
        }
    }

    pub fn net(self, output: Amount, gas: Amount) -> Amount {
        output.saturating_sub(self.cost(gas))
    }
}

impl Default for GasValuation {
    fn default() -> Self {
        Self::ZERO
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SolverConfig {
    pub max_paths: usize,
    pub allocation_steps: u32,
    /// `None` keeps every discovered path and is the exhaustive control mode.
    pub frontier: Option<FrontierConfig>,
    /// Rational conversion from gas units to output-token atomic units.
    pub gas_valuation: GasValuation,
}

impl Default for SolverConfig {
    fn default() -> Self {
        Self {
            max_paths: 4,
            allocation_steps: 64,
            frontier: Some(FrontierConfig::default()),
            gas_valuation: GasValuation::ZERO,
        }
    }
}

/// The mathematical routing pipeline without ingestion, networking, or reporting machinery.
pub struct Solver<'market> {
    topology: &'market MarketTopology,
    router: Router<'market>,
    snapshot: &'market MarketSnapshot,
    config: SolverConfig,
}

impl<'market> Solver<'market> {
    pub fn new(
        topology: &'market MarketTopology,
        snapshot: &'market MarketSnapshot,
        config: SolverConfig,
    ) -> Self {
        assert!(config.max_paths > 0, "max_paths must be positive");
        assert!(
            config.allocation_steps > 0,
            "allocation_steps must be positive"
        );
        Self {
            topology,
            router: Router::new(topology, snapshot),
            snapshot,
            config,
        }
    }

    pub fn solve(&self, paths: &PathSet, amount_in: Amount) -> Option<Solution> {
        let candidates = match self.config.frontier {
            Some(config) => multiscale_frontier(&self.router, paths, amount_in, config),
            None => (0..paths.paths.len())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        };
        let portfolio_solution = maximal_portfolios(paths, &candidates, self.config.max_paths)
            .iter()
            .filter_map(|portfolio| self.allocate(paths, portfolio, amount_in))
            .max_by_key(|solution| solution.net_amount_out);
        let single_solution = candidates
            .iter()
            .filter_map(|&path_index| {
                self.router
                    .replay_path_with_gas(&paths.paths[path_index], amount_in)
                    .map(|(amount_out, gas_estimate)| Solution {
                        block_number: self.snapshot.block_number(),
                        amount_in,
                        amount_out,
                        gas_estimate,
                        net_amount_out: self.config.gas_valuation.net(amount_out, gas_estimate),
                        allocations: [Allocation {
                            path_index,
                            amount_in,
                            amount_out,
                        }]
                        .into(),
                    })
            })
            .max_by_key(|solution| solution.net_amount_out);
        [portfolio_solution, single_solution]
            .into_iter()
            .flatten()
            .max_by_key(|solution| solution.net_amount_out)
    }

    fn allocate(&self, paths: &PathSet, portfolio: &[usize], total: Amount) -> Option<Solution> {
        let allocations = if let [left, right] = portfolio
            && let Some(pair) = crate::certified_v2::allocate_pair(
                self.topology,
                self.snapshot,
                &paths.paths[*left],
                &paths.paths[*right],
                total,
            ) {
            [*left, *right]
                .into_iter()
                .zip(pair)
                .filter(|(_, (amount, _))| !amount.is_zero())
                .map(|(path_index, (amount_in, amount_out))| Allocation {
                    path_index,
                    amount_in,
                    amount_out,
                })
                .collect::<Vec<_>>()
        } else {
            allocate_by_pieces(
                &self.router,
                paths,
                portfolio,
                total,
                self.config.allocation_steps,
                self.config.gas_valuation,
            )?
        };

        let amount_out = allocations.iter().map(|part| part.amount_out).sum();
        let gas_estimate = allocations
            .iter()
            .filter_map(|part| {
                self.router
                    .replay_path_with_gas(&paths.paths[part.path_index], part.amount_in)
                    .map(|(_, gas)| gas)
            })
            .sum();
        Some(Solution {
            block_number: self.snapshot.block_number(),
            amount_in: total,
            amount_out,
            gas_estimate,
            net_amount_out: self.config.gas_valuation.net(amount_out, gas_estimate),
            allocations: allocations.into_boxed_slice(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{GasValuation, Solver, SolverConfig};
    use crate::{
        Amount, TokenId, frontier::FrontierConfig, market::MarketBuilder,
        portfolio::maximal_portfolios, router::PathSet,
    };

    #[test]
    fn canonical_portfolios_do_not_repeat_orderings() {
        let a = TokenId(0);
        let b = TokenId(1);
        let mut builder = MarketBuilder::default();
        builder.add_v2_pool(a, b, Amount::from(1_000_000), Amount::from(1_000_000), 30);
        builder.add_v2_pool(a, b, Amount::from(1_000_000), Amount::from(1_000_000), 30);
        let (topology, _) = builder.finish(1);
        let paths = PathSet::discover(&topology, a, b, 1);
        assert_eq!(maximal_portfolios(&paths, &[0, 1], 2), [vec![0, 1]]);
    }

    #[test]
    fn splits_when_disjoint_paths_improve_output() {
        let a = TokenId(0);
        let b = TokenId(1);
        let mut builder = MarketBuilder::default();
        builder.add_v2_pool(a, b, Amount::from(1_000_000), Amount::from(1_000_000), 30);
        builder.add_v2_pool(a, b, Amount::from(1_000_000), Amount::from(1_000_000), 30);
        let (topology, snapshot) = builder.finish(10);
        let paths = PathSet::discover(&topology, a, b, 1);
        let amount = Amount::from(100_000);
        let single = crate::router::Router::new(&topology, &snapshot)
            .quote_paths(&paths, amount)
            .unwrap();
        let split = Solver::new(
            &topology,
            &snapshot,
            SolverConfig {
                max_paths: 2,
                allocation_steps: 64,
                frontier: Some(FrontierConfig { paths_per_scale: 2 }),
                gas_valuation: GasValuation::ZERO,
            },
        )
        .solve(&paths, amount)
        .unwrap();

        assert_eq!(split.allocations.len(), 2);
        assert!(split.amount_out > single.amount_out);
        assert_eq!(
            split
                .allocations
                .iter()
                .map(|part| part.amount_in)
                .sum::<Amount>(),
            amount
        );
    }

    #[test]
    fn exhaustive_mode_retains_every_path() {
        let a = TokenId(0);
        let b = TokenId(1);
        let mut builder = MarketBuilder::default();
        for reserve in [1_000_000_u64, 900_000, 800_000] {
            builder.add_v2_pool(a, b, Amount::from(reserve), Amount::from(reserve), 30);
        }
        let (topology, snapshot) = builder.finish(1);
        let paths = PathSet::discover(&topology, a, b, 1);
        let solution = Solver::new(
            &topology,
            &snapshot,
            SolverConfig {
                max_paths: 3,
                allocation_steps: 16,
                frontier: None,
                gas_valuation: GasValuation::ZERO,
            },
        )
        .solve(&paths, Amount::from(10_000))
        .unwrap();
        assert!(!solution.allocations.is_empty());
    }

    #[test]
    fn portfolio_solver_never_loses_the_best_single_path() {
        let a = TokenId(0);
        let b = TokenId(1);
        let mut builder = MarketBuilder::default();
        builder.add_v2_pool(a, b, Amount::from(1_000_000), Amount::from(1_000_000), 30);
        builder.add_v2_pool(a, b, Amount::from(900_000), Amount::from(1_100_000), 30);
        let (topology, snapshot) = builder.finish(1);
        let paths = PathSet::discover(&topology, a, b, 1);
        let amount = Amount::from(10_000);
        let single = crate::router::Router::new(&topology, &snapshot)
            .quote_paths(&paths, amount)
            .unwrap();
        let solution = Solver::new(&topology, &snapshot, SolverConfig::default())
            .solve(&paths, amount)
            .unwrap();

        assert!(solution.amount_out >= single.amount_out);
    }

    #[test]
    fn gas_valuation_rounds_only_the_total_cost() {
        let valuation = GasValuation {
            cost_numerator: Amount::from(1),
            cost_denominator: Amount::from(20),
        };
        assert_eq!(valuation.cost(Amount::from(20)), Amount::from(1));
        assert_eq!(valuation.cost(Amount::from(21)), Amount::from(2));
        assert_eq!(
            valuation.net(Amount::from(10), Amount::from(21)),
            Amount::from(8)
        );
    }
}
