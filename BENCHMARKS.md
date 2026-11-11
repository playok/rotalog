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
