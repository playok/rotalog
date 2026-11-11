# rotalog vs Apache rotatelogs

This document records a reproducible comparison with the original Apache
`rotatelogs` binary. It is not a universal performance guarantee; results are
highly dependent on the OS, filesystem, storage, log shape, and kernel cache.

## Test setup

- Date: 2026-11-11
- rotalog: `main` at `7eb28f8` (includes the benchmark and compression changes)
- Original: `/usr/sbin/rotatelogs`, Apache/2.4.67 (Unix)
- Machine: macOS, 10-core CPU, 16 GiB RAM, APFS
- Fixture: synthetic mixed access log, 32 MiB
- Rotation: 8 MiB for the rotation case
- Tool: `hyperfine 2.0.0`, 5 warmup runs, 30 measured runs, warm cache
- Reported values are hyperfine medians
- Output files are removed before each run; benchmark cleanup is not timed

Reproduce with:

```sh
scripts/bench-vs-apache.sh

BENCH_BYTES=134217728 BENCH_RUNS=30 scripts/bench-vs-apache.sh
```

Set `ROTATELOGS_ORACLE=/path/to/rotatelogs` if the original is not installed at
one of the standard paths.

## Results

| Workload | rotalog | Apache rotatelogs | Relative |
| --- | ---: | ---: | ---: |
| plain stdin → file | 12.7 ms | 13.3 ms | ~5% faster |
| `-e` echo to `/dev/null` | 13.0 ms | 13.1 ms | ~equal |
| 8 MiB size rotation | 12.8 ms | 16.0 ms | ~20% faster |
| `-l` local time | 13.5 ms | 13.1 ms | ~3% slower |

Resource usage (medians):

| Workload | rotalog RSS | Apache RSS | rotalog CPU (user/sys) | Apache CPU (user/sys) |
| --- | ---: | ---: | ---: | ---: |
| plain | 9.7 MiB | 2.3 MiB | 2.6 / 10.2 ms | 1.3 / 8.0 ms |
| echo | 9.7 MiB | 2.3 MiB | 2.8 / 10.7 ms | 1.3 / 8.2 ms |
| rotate 8 MiB | 9.8 MiB | 2.3 MiB | 2.7 / 10.5 ms | 1.5 / 11.4 ms |
| local | 9.8 MiB | 2.3 MiB | 2.8 / 10.7 ms | 1.4 / 8.3 ms |

## Interpretation

- Wall-clock throughput is broadly comparable on this warm-cache 32 MiB
  workload. rotalog is slightly faster for plain and rotation workloads in this sample;
  echo is effectively equal and local time is slightly slower.
- rotalog uses more memory (about 9.8 MiB vs 2.3 MiB) because of its bounded
  stdin queue, worker threads, and 64 KiB buffers. That is a deliberate
  concurrency/robustness tradeoff.
- rotalog also uses more user CPU time on the plain path due to the reader
  thread/channel and per-chunk handling. At typical log rates this is not the
  limiting factor; the end-to-end wall times are close.
- The rotation result depends heavily on filesystem state and file count. Treat
  the 19% figure as indicative, not a fixed guarantee.
- Compression is not compared: Apache `rotatelogs` has no `-z` feature. See
  `PERFORMANCE.md` for rotalog's compression benchmarks.
- `--echo-flush buffered`, `--compress-level`, `--compress-jobs`,
  `--fsync-archives`, and `-z` are rotalog extensions.
- Do not compare these numbers across machines without rerunning the script.

## Raw data

`scripts/bench-vs-apache.sh` writes `target/bench-vs-apache/results.json` and
`results.md`. Those files are intentionally not committed because they contain
machine- and filesystem-specific measurements.


## Performance review follow-up (#53–#56)

Measured 2026-11-11 on Darwin 27.2.0 arm64, APFS, with rustc 1.99.1,
release thin LTO and Apache/2.4.67 (`/usr/sbin/rotatelogs`). Baseline is
`4dca8f16337de3b0136056f43a9888f61b21fba9`; candidate includes this PR.
The queue4/queue16 binaries use candidate code with only `QUEUE_DEPTH` changed.
No builds ran concurrently with the 256 MiB measurements below.

The fixture repeats a synthetic access-log line. Each workload uses 3 warmups
and 10 measured runs, removes output files before each run, and includes Apache
as a comparison. Binary order is deterministically shuffled per workload (runs
within each binary remain grouped). These are warm-cache file writes, not
fsync durability measurements. Pipe CPU includes the `cat` producer. RSS is the
hyperfine process-tree peak metric averaged over runs, not a sum of thread
stack reservations.

Reproduce after building baseline and candidate release binaries:

```sh
# Build the baseline in a separate directory/target to avoid Cargo artifact reuse.
mkdir -p /tmp/rotalog-before
git archive 4dca8f16337de3b0136056f43a9888f61b21fba9 | tar -x -C /tmp/rotalog-before
cargo build --release --manifest-path /tmp/rotalog-before/Cargo.toml \
  --target-dir /tmp/rotalog-before/target
cargo build --release
python3 scripts/bench-review.py \
  --binary base=/tmp/rotalog-before/target/release/rotalog \
  --binary candidate=target/release/rotalog \
  --binary apache=/usr/sbin/rotatelogs \
  --bytes 268435456 --runs 10 --output target/perf-review
```

The script requires hyperfine 2.x and writes raw per-run JSON plus `summary.md`.
To compare queue depths, copy candidate sources into separate directories,
change `QUEUE_DEPTH` in `src/runner.rs` to 4 or 16, build with distinct target
directories, and add `--binary queue4=... --binary queue16=...`.

| Workload | Binary | Wall median ms | CPU mean ms | Peak RSS mean MiB |
|---|---|---:|---:|---:|
| file | candidate | 82.88 | 88.73 | 6.42 |
| file | queue4 | 90.19 | 95.33 | 6.17 |
| file | apache | 551.16 | 86.92 | 2.31 |
| file | base | 440.38 | 110.46 | 12.06 |
| file | queue16 | 373.76 | 111.67 | 7.81 |
| echo | queue16 | 88.81 | 103.69 | 7.39 |
| echo | apache | 114.28 | 81.68 | 2.31 |
| echo | queue4 | 93.95 | 105.97 | 6.36 |
| echo | candidate | 86.56 | 100.60 | 6.50 |
| echo | base | 94.08 | 101.14 | 10.60 |
| rotate | base | 131.79 | 111.42 | 10.88 |
| rotate | apache | 157.64 | 124.29 | 2.31 |
| rotate | candidate | 158.77 | 106.84 | 6.83 |
| rotate | queue16 | 135.50 | 116.19 | 7.83 |
| rotate | queue4 | 118.94 | 103.95 | 6.48 |
| local | queue16 | 95.13 | 99.63 | 7.57 |
| local | apache | 304.51 | 271.42 | 2.31 |
| local | candidate | 86.00 | 93.89 | 6.63 |
| local | base | 111.42 | 104.29 | 11.71 |
| local | queue4 | 94.98 | 95.27 | 6.25 |
| pipe | apache | 101.43 | 127.13 | 2.34 |
| pipe | queue4 | 106.02 | 153.15 | 6.60 |
| pipe | candidate | 101.08 | 153.98 | 7.00 |
| pipe | base | 119.63 | 197.13 | 11.15 |
| pipe | queue16 | 110.69 | 165.08 | 7.92 |

Wall time had severe outliers (up to seconds), including inconsistent plain and
local-time results. Neither means nor medians establish a stable speedup on
this machine. CPU results also vary by workload and run order; for example,
Apache still used less CPU on echo and bulk pipe input. Treat these results as
observations, not a claim that candidate beats Apache generally.

The reproducible improvement is lower memory use: queue8 used about 6.4–7.0 MiB
versus baseline's 10.6–12.1 MiB in this run. Queue4 saved only about 0.2–0.4 MiB
more while CPU results were mixed. Queue8 is a conservative compromise providing
twice queue4's burst capacity and less memory than queue16, not a proven global
optimum. Queue payload is capped at 512 KiB; total RSS includes other memory.

A previous simple buffer-return-channel prototype did not consistently improve
CPU across repeated runs, so the allocation/copy path is retained for now. A
bounded pool can be revisited after independent profiling shows a benefit;
changing the channel plus adding buffer reinitialization can erase its gains.

### Fresh-name directory scan microbenchmark

`cargo bench --bench writer -- naming/fresh_size_base` compares the previous
scan-first algorithm with the new check-first algorithm. Both produce the same
absent base name; no file open/write is included. Criterion used 20 samples,
1 second warmup and 2 seconds measurement per case.

| Existing directory entries | Previous scan-first | Candidate check-first |
|---|---:|---:|
| 0 | 12.55 µs | 0.98 µs |
| 10,000 | 2.86 ms | 0.98 µs |

Occupied base names still use a directory scan to choose the next suffix.
Collision, restart and dangling-symlink behavior is covered by tests. This
microbenchmark demonstrates the removed scan cost, not an equivalent end-to-end
throughput multiplier.

### Correctness and limits

- A deterministic stalled-first-completion test fills the maintenance budget
  for worker counts 1/2/4, completes later jobs first, rejects further admission,
  then verifies permit recovery and retention in rotation order.
- Queue-full/disconnection tests verify rejected work does not leak permits or
  consume sequence numbers. Compression-only completion is tested with 1/2/4
  workers; it no longer builds an ordering backlog.
- Integration tests cover a 4 MiB patterned pipe stream across size rotations
  and SIGTERM while an 8 MiB input is backpressured by echo. Written and echoed
  bytes must agree; signal shutdown still provides only the documented grace
  period, not a guarantee to consume all unread input.
- The maintenance budget bounds accepted outstanding work to
  `16 + compress-jobs`, including completed results awaiting retention. It does
  not bound files intentionally skipped on saturation, retained archives, or
  filesystem call duration. Shutdown still waits for accepted work.
- Both the current toolchain and Rust 1.85 pass 80 unit, 23 macOS integration,
  and 5 Apache compatibility tests. Linux-only behavior remains covered by CI.

Additional streaming checks used 128-byte newline-terminated records, 500
request/echo round trips per binary, a 200 µs producer pause, and discarded the
first 10 timing samples. Five rounds with shuffled baseline/candidate/Apache
order gave these ranges across rounds (µs). This is an ad-hoc scheduler-sensitive
latency check, not a throughput benchmark or latency guarantee:

| Binary | p50 range | p95 range | p99 range |
|---|---:|---:|---:|
| Baseline | 12.7–32.6 | 39.4–65.3 | 78.0–199.8 |
| Candidate | 15.0–32.3 | 47.0–98.6 | 75.2–178.3 |
| Apache | 9.1–40.0 | 35.6–57.5 | 71.1–129.0 |

All records were preserved. A separate 8 MiB echo check consumed only 64 KiB
per millisecond; baseline, queue4/8/16 and Apache all preserved both file and
echo bytes and completed in approximately 0.18 seconds. These checks model a
slow stdout consumer; arbitrary slow/network file storage was not simulated.

The Apache rotation workload also has a semantic caveat: in a separate 32 MiB
input/8 MiB threshold check within one timestamp second, Apache preserved the
32 MiB in one timestamp path, while candidate produced four distinct 8 MiB
files. Both preserved bytes, but their file-open/name-collision work differs.
The rotation timings therefore compare the same CLI workload, not identical
file layouts or identical rotation operations.
