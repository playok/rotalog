# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `--compress-level <1-9>` for tuning gzip speed/size; default 3.
- `--compress-jobs <N>` for parallel compression workers; default 1.
- `--fsync-archives <always|data|never>` for archive durability/throughput;
  default `always`.
- 128 KiB compression buffers and a manual copy loop.
- Bounded maintenance queue with explicit queue-full behavior.
- Single post-rotate reaper thread.
- Size-rotation suffix cursor to avoid repeated `.1`..`.N` scans.
- MSRV, dependency audit, and release workflows.
- English README, security policy, contributing guide, and changelog.
- Criterion microbenchmarks and a `hyperfine` end-to-end benchmark script.

### Changed

- gzip temporary files are created with `create_new` and unpredictable names.
- The default gzip level for `-z` changed from 6 to 3 for better throughput;
  use `--compress-level 6` or `9` for smaller archives.
- `-p`/`--program` is rejected with `-z`/`--compress` and `--retain` to avoid
  post-rotate/compression races.
- SIGTERM/SIGINT now use a short drain grace period before shutdown.
- Signal handlers are registered before the first logfile open.
- `retry` backoff uses monotonic `Instant`.
- Compressed files no longer preserve setuid/setgid/sticky bits.
- `-l` now uses DST-aware local civil-time boundaries.
- README now documents the intentional Apache size-threshold difference.

### Fixed

- Compression temp file symlink/TOCTOU exposure.
- Silent success when the stdin reader stops unexpectedly.
- Stale `-t`/`-n` comment and maintenance thread name.
- `align_down` public API panic/overflow edge cases.
- Flaky `-c` integration test; differential tests now reject the
  `rotatelogs -> rotalog` symlink as an oracle.

## [0.1.0] - 2026-11-11

### Added

- Initial Rust implementation of Apache httpd `rotatelogs`-compatible log
  rotation.
- Time-based, size-based, and combined rotation.
- `-l`, `-L`, `-p`, `-f`, `-D`, `-t`, `-T`, `-e`, `-c`, `-n`, and `-v`.
- Asynchronous gzip compression (`-z`) and retention (`--retain`).
- Differential CLI tests against the original `rotatelogs` binary.
