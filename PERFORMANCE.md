# Performance

This document records the benchmark harness and an example baseline for
`rotalog`. Performance numbers are hardware- and filesystem-dependent; use
them for before/after comparisons on the same machine, not as absolute targets.

## Microbenchmarks

Install no extra tooling beyond the crate's dev dependencies:

```sh
cargo bench --bench writer
```

The microbenchmarks use `/dev/null` for the write-only cases, so they measure
`Writer::write` overhead rather than disk bandwidth. Size-rotation benchmarks
write to a temporary directory and therefore include file creation, path
resolution, rotation, and close costs. Setup and temporary-directory cleanup are
excluded from the timed routine by Criterion's `iter_batched`.

Covered cases:

- `writer/write/{64,1024,65536}` — per-call write overhead and throughput
- `writer/write/local_time_64KiB` — `-l` steady-state write path
- `writer/size_rotation/{64KiB,1MiB,64MiB}` — rotation density and file creation

Run a subset with Criterion filters:

```sh
cargo bench --bench writer -- write
cargo bench --bench writer -- size_rotation
```

## End-to-end benchmarks

`scripts/bench.sh` generates a representative log fixture and compares:

- plain stdin → file
- `-e` echo to `/dev/null`
- 64 KiB size rotation
- `-z` compression with 32 MiB rotation
- `-l` local time

Prerequisites:

```sh
cargo install hyperfine
# or: brew install hyperfine
```

Run:

```sh
scripts/bench.sh

BENCH_BYTES=536870912 BENCH_RUNS=10 scripts/bench.sh
```

Results are written to `target/bench/results.json` and
`target/bench/results.md`. The default fixture is 128 MiB and 5 runs; increase it
for stable throughput comparisons.

## Example baseline

Recorded from a warm-cache run on macOS, 10-core CPU, 16 GiB RAM, APFS. This is
an example, not a guarantee.

### `cargo bench --bench writer`

| Case | Time | Throughput |
| --- | ---: | ---: |
| write 64 B | ~669 ns | ~91 MiB/s |
| write 1 KiB | ~665 ns | ~1.43 GiB/s |
| write 64 KiB | ~668 ns | ~91 GiB/s |
| local-time write 64 KiB | ~668 ns | ~91 GiB/s |
| size rotation 64 KiB | ~225 µs | ~278 MiB/s |
| size rotation 1 MiB | ~2.17 ms | ~460 MiB/s |
| size rotation 64 MiB | ~141 ms | ~455 MiB/s |

Interpretation:

- The per-call overhead is roughly **0.67 µs**; with 64 KiB chunks this is
  negligible relative to throughput.
- `/dev/null` microbenchmarks show that the basic write path is not the first
  optimization target.
- Rotation-heavy workloads are dominated by file creation/path/open work, not
  by the `write` syscall itself.
- The 64 MiB rotation result has high variance because it writes a large amount
  of data to the filesystem. Use the end-to-end script for a more realistic
  comparison.

### End-to-end

Run `scripts/bench.sh` on the target machine and keep the generated
`results.md` with the PR that changes performance-sensitive code. Do not
compare results collected on different machines or filesystems.

## Benchmarking policy

- Add or update a benchmark for every performance-sensitive change.
- Report before/after medians and the benchmark machine/filesystem.
- Prefer representative log fixtures over `/dev/zero` or `/dev/urandom`.
- Treat warm-cache and cold-cache results differently.
- CI compiles benchmarks with `cargo bench --no-run`; it does not run long
  benchmarks on every pull request.
