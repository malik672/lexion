# Artifacts

Generated research and benchmark evidence lives here rather than beside production code.

- `benchmarks/runs/` contains reports emitted by `algorithm_bench`.
- `benchmarks/comparisons/` contains external-router comparison datasets.
- `benchmarks/gas-audit/` contains gas-audit reports and intermediate data.

These files are reproducible outputs and are ignored by Git. Keep durable methodology and reusable
configuration in source control; keep individual run output here.
