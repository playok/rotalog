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
| line-sized file write 64 B | ~1.39 µs | ~44 MiB/s |
| size rotation 64 KiB | ~225 µs | ~278 MiB/s |
| size rotation 1 MiB | ~2.17 ms | ~460 MiB/s |
| size rotation 64 MiB | ~141 ms | ~455 MiB/s |

Interpretation:

- The per-call overhead is roughly **0.67 µs** for `/dev/null`; with 64 KiB
  chunks this is negligible relative to throughput.
- A line-sized 64 B write to a real temporary file costs roughly **1.39 µs**
  (about 44 MiB/s). File buffering could reduce syscalls, but it would need a
  flush policy that preserves `tail -F` visibility and rotation semantics.
  Current decision: keep unbuffered file writes by default and revisit if a
  real line-sized workload shows a bottleneck.
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

## Compression level

`-z` defaults to gzip level 3. On a synthetic 64 MiB mixed log with 32 MiB
rotation, a local smoke run produced:

| Level | User time | Archive size |
| --- | ---: | ---: |
| 1 | ~0.09 s | ~7.3 MiB |
| 3 | ~0.14 s | ~5.6 MiB |
| 6 | ~0.31 s | ~5.0 MiB |
| 9 | ~0.48 s | ~4.9 MiB |

Level 1 is the fastest, level 3 is the default balance, and 6-9 are useful when
archive size matters more than CPU. Wall time also depends on fsync and storage;
the table is illustrative rather than a strict benchmark.

`--fsync-archives always|data|never` controls the archive durability barrier.
`always` (default) calls `sync_all`, `data` calls `sync_data`, and `never` skips
fsync. The last two can improve throughput on slow storage but weaken crash
durability; `scripts/bench.sh` includes a `never` comparison.

A local smoke run on a 64 MiB mixed log with 32 MiB rotation showed roughly
0.47 s for `always` vs 0.18 s for `data`/`never`, with user time unchanged at
about 0.14 s. This illustrates that fsync can dominate wall time; it is not a
substitute for repeated measurements on the target storage.

`--compress-jobs N` controls parallel compression workers (default 1, max 32).
Increase it when compression lags rotation on a multi-core host. Compression
still competes with the logger for CPU and I/O; measure with the target log
volume before choosing a value above 1.

Local smoke run on a 128 MiB mixed log, 32 MiB rotation, level 3, and
`--fsync-archives never`:

| Jobs | Wall time | User time |
| ---: | ---: | ---: |
| 1 | ~0.73 s | ~0.43 s |
| 2 | ~0.32 s | ~0.42 s |
| 4 | ~0.22 s | ~0.43 s |

The user time stays roughly constant while wall time drops, confirming that
compression is parallelized. Real gains depend on available cores, storage, and
fsync policy.

`--echo-flush buffered` batches stdout writes up to 64 KiB or 100 ms. This can
reduce syscall overhead for high-rate `-e` pipelines, but adds up to 100 ms of
downstream latency. The default remains `immediate`.

A 128 MiB smoke run with 5 runs measured ~110 ms for immediate echo vs ~75 ms
for buffered echo (about 32% faster), with high run-to-run variance. Use
`scripts/bench.sh` on the target pipeline before changing the default.

Rotation-heavy micro-optimizations in the current implementation:

- reuse the period offset instead of doing two local-time lookups
- keep a suffix cursor and skip the extra base `stat`
- cache the last created `-D` parent directory
- try a hard link before canonicalizing the target, and short-circuit
  canonicalization in `same_file`
- parse the strftime pattern once per writer instead of on every rotation
- scan the directory once to initialize the suffix cursor after restart
- use a crossbeam bounded channel for stdin→writer transfer

64 KiB chunk allocation remains; a 512 MiB warm-cache run showed ~0.01 s user
time, so buffer pooling is deferred unless a line-sized workload proves it
necessary.

## Benchmarking policy

- Add or update a benchmark for every performance-sensitive change.
- Report before/after medians and the benchmark machine/filesystem.
- Prefer representative log fixtures over `/dev/zero` or `/dev/urandom`.
- Treat warm-cache and cold-cache results differently.
- CI compiles benchmarks with `cargo bench --no-run`; it does not run long
  benchmarks on every pull request.
