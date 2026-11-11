# rotalog

[한국어](README.md) | English

`rotalog` is a from-scratch Rust implementation of log rotation for
Apache-style pipes. It follows the behavior and command-line semantics of
Apache httpd's `rotatelogs` while adding safer collision handling, asynchronous
gzip/retention, and a memory-safe threaded design.

```apache
CustomLog "|/usr/local/bin/rotalog /var/log/httpd/access.log 86400" combined
ErrorLog  "|/usr/local/bin/rotalog /var/log/httpd/error.log 100M"
```

## Highlights

- Time-based, size-based, and combined rotation
- Apache-compatible CLI: `-l`, `-L`, `-p`, `-f`, `-D`, `-t`, `-T`, `-e`, `-c`,
  `-n`, `-v`
- `strftime` filenames, UTC offsets, local time, and atomic current-log links
- Asynchronous gzip (`-z`) and retention (`--retain`)
- Bounded stdin queue, single writer, no `unsafe`
- Graceful SIGTERM/SIGINT handling with a short drain grace period
- Same-name rotations never overwrite an existing archive; `.1`, `.2` are used
- Rotation failures keep writing to the old file and retry with monotonic backoff

## Build and Install

```sh
cargo build --release
# target/release/rotalog

cargo install --path .
# or
install -m 0755 target/release/rotalog /usr/local/bin/rotalog

# Optional compatibility name for existing httpd configs:
ln -s /usr/local/bin/rotalog /usr/local/bin/rotatelogs
```

Dependencies: `chrono` (time zones/strftime), `flate2` (gzip), `signal-hook`
(signals).

## Usage

```text
rotalog [OPTIONS] <logfile> <rotationtime|filesize> [<filesize>] [<offset>]
```

- `<logfile>`: output path or `strftime(3)` pattern (detected by `%`)
- `<rotationtime>`: seconds between rotations
- `<filesize>`: maximum size with uppercase `B`, `K`, `M`, or `G`
- `<offset>`: UTC offset in minutes, last positional argument
- Time and size can be combined: `3600 100M`
- Options must precede the logfile, matching Apache's POSIX getopt behavior.
  `--` ends option parsing.

### Options

| Option | Description |
| --- | --- |
| `-l`, `--local-time` | Use the system local time zone |
| `-L`, `--link <path>` | Atomically point a link at the current file |
| `-p`, `--program <path>` | Run `path <new> [<previous>]` after opening a file |
| `-f`, `--force` | Open the first file immediately |
| `-D`, `--create-dirs` | Create missing parent directories |
| `-t`, `--truncate` | Truncate the same path instead of rotating |
| `-T`, `--truncate-rotated` | Truncate re-opened rotated targets |
| `-e`, `--echo` | Also copy the stream to stdout |
| `--echo-flush <immediate\|buffered>` | Echo flush policy; default `immediate` |
| `-c`, `--create-empty` | Create empty files on each time interval |
| `-n`, `--circular <count>` | Use `base`, `base.1`, ... `base.(count-1)` |
| `-v`, `--verbose` | Print configuration and file operations to stderr |
| `-h`, `--help` | Show help |
| `-V`, `--version` | Show version |

Extensions over Apache rotatelogs:

| Option | Description |
| --- | --- |
| `-z`, `--compress` | Gzip rotated files in the background |
| `--compress-level <1-9>` | Gzip level for `-z`; default 3 (1=fast, 6-9=smaller) |
| `--compress-jobs <N>` | Parallel compression workers for `-z`; default 1, max 32 |
| `--fsync-archives <always\|data\|never>` | Archive fsync policy; default `always` |
| `--retain <count>` | Keep only the newest `count` archives from this process |

## Compatibility and Differences

The CLI grammar and `-v` configuration are checked against a real Apache
`rotatelogs` binary when one is available. Valid documented forms match.

Intentional differences:

- Size rotation happens before a write would exceed the limit; Apache rotates
  after the limit is exceeded.
- Coarse `strftime` patterns never overwrite a previous archive; `.1`, `.2` are
  appended.
- Rotation-open failures keep the old file and retry; Apache truncates it and
  records lost messages.
- Malformed `atoi()`-style inputs (lowercase units, trailing garbage,
  out-of-range offsets, unknown long options) are rejected explicitly.
- `-h`, `-V`, long options, `-z`, and `--retain` are extensions.
- `-l` uses local civil-time boundaries, including DST-aware day/hour starts;
  this differs from Apache only on DST transition days.
- A write failure to the current file exits with status 2; a rotation-open
  failure keeps the old file and retries. The maintenance queue is bounded to
  16 files; queue-full warnings are rate-limited to once per 5 seconds and a
  total is logged at shutdown. Compression can run on multiple workers, and
  retention is applied in rotation order. SIGTERM gives in-flight data a short
  drain grace period.

`-p` cannot be combined with `-z`/`--retain` because the post-rotate program may
read the previous file asynchronously. `-z` is also incompatible with `-t`,
`-T`, and `-n`. `--compress-level` requires `-z`; the default is 3, with 1 for
speed and 6-9 for smaller archives. `--compress-jobs` also requires `-z` and
defaults to 1; increase it on multi-core hosts when compression lags rotation.
`--fsync-archives` also requires `-z`: `always` (default) uses `sync_all`,
`data` uses `sync_data`, and `never` skips fsync at the cost of crash
durability. `--echo-flush` requires `-e`; `immediate` (default) flushes each
write, while `buffered` batches up to 64 KiB or 100 ms.

## Performance Benchmarks

```sh
cargo bench --bench writer
scripts/bench.sh          # requires hyperfine
scripts/bench-vs-apache.sh # compare against the original rotatelogs
```

- Criterion: `write` 64 B/1 KiB/64 KiB, `-l`, size rotation 64 KiB/1 MiB/64 MiB
- end-to-end: plain/echo/rotation/compression/local-time
- Apache comparison methodology and results: `BENCHMARKS.md`
- Baseline and interpretation: `PERFORMANCE.md`
- CI compiles benchmarks with `cargo bench --no-run`

## Tests

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

- 76 unit tests
- 22 integration tests on Linux (21 on macOS, including one Linux-only
  write-failure test)
- 5 differential compatibility tests (51-case argv matrix plus malformed,
  extension, oracle-identity, and behavior checks)

Run the differential suite against a specific oracle:

```sh
ROTATELOGS_ORACLE=/usr/sbin/rotatelogs \
  cargo test --test original_cli_compat -- --nocapture
```

## License

MIT. See `LICENSE`. Apache httpd `rotatelogs` is acknowledged in
`CREDITS.md`; no Apache C source code is included.
