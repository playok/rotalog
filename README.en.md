# rotalog

[한국어](README.md) | [English](README.en.md)

A from-scratch Rust implementation of Apache-compatible log rotation.

The project follows the **features and option semantics** of Apache httpd's
`support/rotatelogs.c`, with an implementation designed around Rust.

Option letters and positional argument syntax are preserved so rotalog can be
used directly in existing `httpd.conf` `CustomLog`/`ErrorLog` pipes. Internally,
C's global state, APR pools, and `exit()`-oriented control flow are replaced by
an ownership-based state machine, explicit error propagation, and safe
background maintenance threads.

```apache
CustomLog "|/usr/local/bin/rotalog /var/log/httpd/access.log 86400" combined
ErrorLog  "|/usr/local/bin/rotalog /var/log/httpd/error.log 100M"
```

## What Changed in the Rust Implementation

- **Prevent accidental overwrites:** when size rotation occurs several times
  within a coarse pattern such as `%Y-%m-%d`, Apache may overwrite the same
  file. This implementation preserves existing logs with `.1`, `.2` suffixes.
- **Preserve logs after rotation failures:** if the new file cannot be opened,
  keep writing to the old file and retry every 5 seconds. Apache truncates the
  existing file, writes a lost-message notice, and continues.
- **Atomic link updates:** `-L` creates a temporary link, then replaces the old
  link with `rename`, avoiding a missing-link interval for `tail -F`. A symbolic
  link is used if the filesystem does not allow a hard link.
- **Asynchronous gzip and retention:** `-z` and `--retain` run on separate
  maintenance threads so compression does not directly block the httpd pipe.
- **Graceful shutdown:** on `SIGTERM`/`SIGINT`, write queued logs, flush, and
  exit with code 0.
- **Safe concurrency:** a stdin reader and a single writer communicate through
  a bounded channel with 8 × 64 KiB slots. Queue memory is bounded, and file
  state needs no locks.
- **No `unsafe`:** the entire crate uses `#![forbid(unsafe_code)]`.
- **Deterministic tests:** rotation takes the current timestamp as an argument,
  allowing time, size, and DST boundary tests without sleeping.

## Build and Install

GitHub Releases on `v<version>` tags include OS/CPU archives and SHA-256 checksums:

| Platform | Archive target |
|---|---|
| Linux x86_64 | `x86_64-unknown-linux-musl` |
| Linux ARM64 | `aarch64-unknown-linux-musl` |
| macOS Intel (11+) | `x86_64-apple-darwin` |
| macOS Apple Silicon (11+) | `aarch64-apple-darwin` |

Linux builds use static musl linkage to avoid a glibc version dependency; this
is not certification of every Linux distribution/version. Windows binaries are
not currently provided. Archives contain `rotalog`, the license and READMEs.

```sh
# Download the matching archive and checksum from GitHub Releases first.
shasum -a 256 -c rotalog-v0.1.1-x86_64-unknown-linux-musl.tar.gz.sha256
tar -xzf rotalog-v0.1.1-x86_64-unknown-linux-musl.tar.gz
sudo install -m 0755 rotalog /usr/local/bin/rotalog
```

Maintainers: push a `v<version>` tag matching `Cargo.toml`. The workflow tests,
builds and verifies all four targets before attaching them to a GitHub Release.
PRs and manual runs only validate builds and upload Actions artifacts. Portable
release settings are used, without `target-cpu=native`. crates.io publishing
requires the existing token and only runs for non-prerelease tags.

To build from source:

```sh
cargo build --release
# target/release/rotalog

cargo install --path .
# or
install -m 0755 target/release/rotalog /usr/local/bin/rotalog

# Optional compatibility name for existing httpd configs:
ln -s /usr/local/bin/rotalog /usr/local/bin/rotatelogs
```

Runtime dependencies: `chrono` (time zones/strftime), `crossbeam-channel`
(bounded queues), `flate2` (gzip), and `signal-hook` (signals).

## Usage

```text
rotalog [OPTIONS] <logfile> <rotationtime|filesize> [<filesize>] [<offset>]
```

- `<logfile>`: output path or `strftime(3)` pattern, detected by `%`.
- `<rotationtime>`: integer rotation interval in seconds.
- `<filesize>`: maximum size with an uppercase `B`, `K`, `M`, or `G` suffix,
  such as `512K`, `100M`, or `2G`.
- `<offset>`: UTC offset in minutes (`-840`..`840`), allowed only as the last
  positional argument.
- Time and size can be combined: `3600 100M`.
- Options must appear **before the first positional argument (logfile)**,
  matching Apache. `--` ends option parsing. Arguments after the logfile are
  not interpreted as options.

### Main Options

| Option | Description |
| --- | --- |
| `-l`, `--local-time` | Use the system local time zone |
| `-L`, `--link <path>` | Atomically point a link at the current file |
| `-p`, `--program <path>` | Run `path <new> [<previous>]` after opening a file |
| `-f`, `--force` | Open the first file immediately |
| `-D`, `--create-dirs` | Create missing parent directories |
| `-t`, `--truncate` | Truncate instead of rotating; `%` patterns still expand |
| `-T`, `--truncate-rotated` | Truncate rotated targets when opened, excluding the initial file |
| `-e`, `--echo` | Also copy the stream to stdout |
| `--echo-flush <immediate\|buffered>` | Echo flush policy; default `immediate` |
| `-c`, `--create-empty` | Create empty files on each time interval |
| `-n`, `--circular <count>` | Use `base`, `base.1`, ... `base.(count-1)` |
| `-v`, `--verbose` | Print configuration and file operations to stderr |
| `-h`, `--help` | Show help |
| `-V`, `--version` | Show version |

### Extensions over Apache rotatelogs

| Option | Description |
| --- | --- |
| `-z`, `--compress` | Gzip rotated files in the background |
| `--compress-level <1-9>` | Gzip level for `-z`; default 3 (1=fast, 6-9=smaller) |
| `--compress-jobs <N>` | Parallel compression workers for `-z`; default 1, max 32 |
| `--fsync-archives <always\|data\|never>` | Archive fsync policy; default `always` |
| `--retain <count>` | Keep only the newest `count` archives from this process |

Constraints:

- `-z` cannot be combined with `-t`, `-T`, or `-n`: reusing or immediately
  truncating names makes the compression target ambiguous.
- `--retain` cannot be combined with `-n`, `-t`, or `-T`.
- `-p` cannot be combined with `-z`/`--compress` or `--retain`, preventing races
  between a program reading the previous file and compression/deletion.
- `--compress-level` requires `-z`. The default is 3; 1 favors speed and 6–9
  favor smaller archives.
- `--compress-jobs` requires `-z`. The default is 1; up to 32 workers can run
  compression in parallel to increase throughput.
- `--fsync-archives` requires `-z`: `always` (default) uses `sync_all`, `data`
  uses `sync_data`, and `never` skips fsync. `data`/`never` weaken archive
  durability guarantees for crashes or power loss.
- `--echo-flush` requires `-e`: `immediate` (default) flushes every chunk;
  `buffered` batches up to 64 KiB or 100 ms.
- `-c` only has an effect with a time interval. As in Apache, it is accepted
  but inert for size-only rotation. The first empty file is created after one
  full interval from startup.
- `-l` and `<offset>` cannot be used together.
- As in Apache, `-t` takes precedence over `-T`/`-n`. When combined, those
  options are ignored and a single file is truncated.
- `-n` cannot be used with a `strftime` pattern.
- Size units must be uppercase. Apache's `atoi()` behavior can interpret
  lowercase `b/k/m/g` as time values; rotalog rejects such ambiguous input.

Additional notes:

- A `-p` program inherits rotalog's stdin/stdout/stderr, matching Apache. If
  that program reads stdin, it can consume data from the logging pipe.
- Use a `strftime` pattern fine enough to distinguish rotation intervals. For
  example, daily rotation with only `%Y` repeats the same name and creates
  `.1`, `.2` suffixes.
- When a `-p` script reads a file reused by `-n` or truncated by `-t`, the
  contents may already have changed by the time the script runs. Copy any
  required data promptly.

## Filename Rules

1. Names containing `%` are expanded as `strftime` patterns:
   `access-%Y%m%d.log` → `access-20261111.log`.
2. With `-n N`, names cycle through `logfile`, `logfile.1`, …, `logfile.(N-1)`.
3. With `-t`, a name without `%` is used unchanged.
4. Otherwise, use `logfile.<10-digit adjusted Unix timestamp>`, aligned to the
   start of the time-rotation interval.

Example:

```sh
printf 'hello\n' | rotalog /var/log/access.log 86400
# /var/log/access.log.1794355200  (aligned to a daily boundary)
```

For UTC/fixed offsets, intervals are aligned by flooring “UTC + offset” using
integer division, as in Apache. With `-l`, boundaries are computed in local
civil time, keeping midnight/hour boundaries aligned across DST transitions.
This deliberately differs from Apache's adjusted-epoch approach on DST
transition days.

A size rotation within a time interval names the new file using the **actual
rotation time**, rather than the interval start. If the pattern repeats the
same name, `.1`, `.2` suffixes preserve existing files without overwriting them.

```text
access.20261111.log
access.20261111.log.1
access.20261111.log.2
```

### Date and Time Format Specifiers

Use the `strftime` pattern **`%Y-%m-%d`**, rather than the literal string
`yyyy-mm-dd`, in filenames. Examples below format
`2026-11-12 15:04:05 +09:00`. `-l` selects the system local time zone; otherwise
UTC or the fixed offset in the final positional argument is used.

| Specifier | Meaning | Example output |
| --- | --- | --- |
| `%Y` | Four-digit year | `2026` |
| `%y` | Two-digit year | `26` |
| `%m` | Month (`01`–`12`) | `11` |
| `%d` | Day of month (`01`–`31`) | `12` |
| `%H` | Hour, 24-hour clock (`00`–`23`) | `15` |
| `%I` | Hour, 12-hour clock (`01`–`12`) | `03` |
| `%p` | AM/PM marker (English) | `PM` |
| `%M` | Minute (`00`–`59`) | `04` |
| `%S` | Second (two digits) | `05` |
| `%j` | Day of year (`001`–`366`) | `316` |
| `%u` | Weekday number (Monday=1, Sunday=7) | `4` |
| `%a` | Abbreviated weekday name (English) | `Thu` |
| `%A` | Full weekday name (English) | `Thursday` |
| `%b` | Abbreviated month name (English) | `Nov` |
| `%B` | Full month name (English) | `November` |
| `%F` | Same as `%Y-%m-%d` | `2026-11-12` |
| `%R` | Same as `%H:%M` | `15:04` |
| `%T` | Same as `%H:%M:%S` | `15:04:05` |
| `%z` | UTC offset without a colon | `+0900` |
| `%:z` | UTC offset with a colon | `+09:00` |
| `%s` | Unix timestamp in UTC seconds | `1794463445` |
| `%%` | Literal `%` | `%` |

**`%m` means month; `%M` means minute.** To avoid the colons produced by `%R`,
`%T`, or `%:z`, use `%H-%M-%S` and `%z` in filenames. Month and weekday names
are English regardless of the system locale. This table covers common
specifiers; see the dependency's
[chrono strftime documentation](https://docs.rs/chrono/0.4.45/chrono/format/strftime/index.html)
for the full syntax. Unsupported specifiers (such as `%Q`) and a trailing
standalone `%` are rejected before execution.

### Filename Examples by Rotation Interval

The examples use the same timestamp and `+09:00` time zone. Set the filename
pattern and rotation interval separately: adding a date pattern does not
change the rotation interval.

| Purpose | Filename pattern | Interval (seconds) | Example filename |
| --- | --- | ---: | --- |
| Daily | `catalina.out.%Y-%m-%d.log` | `86400` | `catalina.out.2026-11-12.log` |
| Daily, compact date | `catalina.out.%Y%m%d.log` | `86400` | `catalina.out.20261112.log` |
| Hourly | `catalina.out.%Y-%m-%d_%H.log` | `3600` | `catalina.out.2026-11-12_15.log` |
| Every minute | `catalina.out.%Y-%m-%d_%H-%M.log` | `60` | `catalina.out.2026-11-12_15-04.log` |
| Every second | `catalina.out.%Y-%m-%d_%H-%M-%S.log` | `1` | `catalina.out.2026-11-12_15-04-05.log` |
| Monthly directories, daily files | `logs/%Y/%m/catalina.out.%Y-%m-%d.log` | `86400` | `logs/2026/11/catalina.out.2026-11-12.log` |

Use `-D` to create directories included in the pattern automatically.
Time-rotation filenames use the interval's start time; size-rotation filenames
use the actual rotation time. Collisions caused by a coarse pattern receive
`.1`, `.2` suffixes.

## How It Works

### Rotation Decision

Immediately before writing, perform these checks in order:

1. Open a file if none is open.
2. Rotate for time when a time limit exists and the current adjusted time is
   at or beyond the interval end.
3. Rotate for size when a size limit exists and the current size plus the next
   chunk exceeds that limit.

Normal chunks therefore do not cross the limit. If one stdin read is larger
than the limit, that entire chunk is written, so a file can exceed the limit
by that amount. This preserves chunk atomicity for tests and pipe processing.
Apache rotates on the write after the limit has already been exceeded;
rotalog rotates before the next chunk would exceed it. Both allow up to one
chunk of error, with rotalog staying closer to the configured maximum.

### Rotation Sequence

1. Compute the new filename, adding a collision suffix if needed.
2. With `-D`, create the parent directory.
3. Open the new file using `O_APPEND` or `O_TRUNC`.
4. Atomically update the `-L` link.
5. Start the `-p` program asynchronously.
6. Close the previous file handle.
7. If `-z`/`--retain` is enabled, send the previous path to maintenance.
8. Replace the current file state.

If opening the new file fails, continue writing to the previous file and retry
after 5 seconds. Link and `-p` failures only produce diagnostics; logging
continues.

A write failure to the active file, such as `ENOSPC` or `EIO`, prints a diagnostic
and exits with code 2 to preserve data consistency. The parent process
(httpd/systemd) is responsible for restarts and alerts. This policy avoids
retries that could duplicate a partially written chunk.

As in Apache, `-c` does not immediately create the current interval's empty
file. Empty files begin after a full interval from startup, then follow interval
boundaries. With `-f` as well, the initial file is opened immediately and later
intervals rotate normally.

### Thread Model

```text
httpd ──stdin──▶ reader thread ──bounded channel──▶ writer thread ──▶ logfile
                                                          │
                                                          ├──(-e)──▶ stdout
                                                          │
                                                          └──retired path──▶ bounded queue
                                                                              ├─▶ compress worker 1..N
                                                                              └─▶ retention coordinator
```

- The reader reads up to 64 KiB at a time and sends chunks through
  `crossbeam_channel::bounded(8)`. A slow writer applies backpressure naturally.
  Queued payload is at most 512 KiB; this is not a total RSS limit.
- Only the writer owns rotation state and file handles, so those need no locks.
- The maintenance pool compresses files in parallel; the retention coordinator
  deletes completed archives in rotation order. The input queue holds 16 jobs,
  and all queued, running, and completed-but-waiting work is capped at
  `16 + compress-jobs`. Saturation leaves that file without compression or
  retention and aggregates warnings. Shutdown waits for accepted work to finish.
- `SIGTERM`/`SIGINT` are delivered through `signal-hook` flags. While waiting
  for input, the writer checks at intervals of up to 200 ms, then allows a
  250 ms grace period for in-flight data before exiting. Data still unread in
  the OS pipe beyond that period is not guaranteed.

## Apache httpd Configuration Examples

```apache
# Daily rotation, UTC
CustomLog "|/usr/local/bin/rotalog /var/log/httpd/access.log 86400" combined

# Local time with date-based filenames
CustomLog "|/usr/local/bin/rotalog -l /var/log/httpd/access.%Y%m%d.log 86400" combined

# Combined time and size limits
CustomLog "|/usr/local/bin/rotalog /var/log/httpd/access.log 3600 100M" combined

# Current-log link for tail -F
CustomLog "|/usr/local/bin/rotalog -L /var/log/httpd/access-current.log /var/log/httpd/access.log 86400" combined

# Seven circular files
CustomLog "|/usr/local/bin/rotalog -n 7 /var/log/httpd/access.log 100M" combined

# Gzip and retention of the newest 14 archives (extension)
CustomLog "|/usr/local/bin/rotalog -z --retain 14 /var/log/httpd/access.%Y%m%d.log 100M" combined
```

`-p` executes the program directly without a shell. Use a wrapper script when
arguments are needed:

```sh
#!/bin/sh
exec /usr/local/bin/post-rotate --tag httpd "$@"
```

## Tomcat `catalina.out` Configuration

Tomcat 9's `CATALINA_OUT_CMD` forwards the JVM's stdout and stderr to rotalog.
This example combines **daily rotation at local midnight**, **`yyyy-mm-dd`
filenames**, and **gzip compression of rotated files**.

Install rotalog at `/usr/local/bin/rotalog` and give the Tomcat service account
write access to `$CATALINA_BASE/logs`. Stop Tomcat, add the following to
`$CATALINA_BASE/bin/setenv.sh`, then start Tomcat again:

```sh
# Input FIFO for Tomcat's stdout/stderr; catalina.sh creates it automatically.
export CATALINA_OUT="$CATALINA_BASE/logs/catalina.pipe"

# Keep this command on one line. Paths must not contain spaces.
export CATALINA_OUT_CMD="/usr/local/bin/rotalog -l -f -z -L $CATALINA_BASE/logs/catalina.current $CATALINA_BASE/logs/catalina.out.%Y-%m-%d.log 86400"
```

- `-l`: rotate at midnight in the system's local time zone.
- `-f`: open the current date's log immediately at startup.
- `-z`: gzip retired files asynchronously. Omit it to disable compression.
- `-L`: point `catalina.current` at the active log for date-independent access.
- `%Y-%m-%d`: format dates as `2026-11-12`.
- `86400`: rotate daily, aligned to date boundaries rather than 24 hours after startup.

Example files:

```text
logs/catalina.pipe                       # Input FIFO; do not use it to read logs
logs/catalina.out.2026-11-11.log.gz       # Previous date's compressed log
logs/catalina.out.2026-11-12.log          # Active log
logs/catalina.current                    # Link to the active log
```

Using a separate `catalina.pipe` FIFO avoids conflicts with an existing regular
`catalina.out` file. Follow the active log with
`tail -F "$CATALINA_BASE/logs/catalina.current"`. The active file stays
uncompressed, including after Tomcat shuts down.

By default, the first log data after midnight triggers rotation to the next
date's file. Add `-c` to create daily files even when no data arrives. To rotate
in Korean time, set the Tomcat service's system time zone or `TZ` environment
variable to `Asia/Seoul`.

This configuration applies when starting Tomcat with `startup.sh` or
`catalina.sh start`. Services using `catalina.sh run` or invoking Java directly
need a separate configuration to pipe stdout and stderr into rotalog.

The same connection method was tested with Tomcat 9.0.123 for 60-second
rotation, gzip compression, and clean shutdown. The daily example uses an
interval of `86400` and the filename pattern `%Y-%m-%d`.

## Original CLI Compatibility Verification

`tests/original_cli_compat.rs` uses a real Apache `rotatelogs` binary as an
oracle, running the same argv against both implementations and comparing results.

- Oracle search order: `ROTATELOGS_ORACLE` → `/usr/sbin/rotatelogs` →
  `/usr/local/sbin/rotatelogs` → Homebrew httpd paths → `rotatelogs` on `PATH`.
- If no oracle exists, tests print a skip message and succeed, allowing
  `cargo test` to run on CI without Apache.
- Coverage: 51 common argv cases, 8 malformed inputs, 5 extension cases,
  3 behavior comparisons, and 1 oracle-identity check.
- Compared results include acceptance/rejection and exit codes; effective
  `-v` settings (time, size, offset, local time, `strftime`, `-f`, `-D`, `-c`,
  `-p`, and logfile); and actual files/stdout for `-n`, `-t`, `-L`, and `-e`.
- Valid forms cover time only, size only, time+size in either order, and a
  final UTC offset; `-l`, `-f`, `-D`, `-e`, `-t`, `-T`, `-n`, `-L`, `-p`,
  `-c`, and `-v`; attached values (`-n3`, `-Lpath`), combined flags (`-lveD`),
  `--`, `strftime` patterns, and `-t` precedence over `-T`/`-n`.
- `-c` creates empty files with time rotation and, as in Apache, is accepted
  but inert with size-only rotation.
- Rejected forms include missing/extra arguments, duplicate time/size limits,
  zero limits, `-l` with an offset, zero/negative/nonnumeric `-n`, a pattern
  combined with `-n`, unknown short options, and missing option values.

Run:

```sh
# Automatic oracle discovery
cargo test --test original_cli_compat -- --nocapture

# Explicit oracle
ROTATELOGS_ORACLE=/usr/sbin/rotatelogs \
  cargo test --test original_cli_compat -- --nocapture
```

### Intentional Differences

Apache's `atoi()` and byte scanner silently convert malformed inputs. This
implementation rejects the same inputs with explicit errors.

| Input | Apache | rotalog |
| --- | --- | --- |
| `5m` | 5 seconds (`atoi`) | Rejected; units must be uppercase |
| `5MB` | 5 bytes (`B` is found first) | Rejected |
| `60abc` | 60 seconds | Rejected |
| `60 abc` (last argument) | Offset 0 | Rejected |
| `60 99999` | Offset 99999 minutes | Rejected; out of range |
| `-n 3x` | `-n 3` | Rejected |
| `--anything` | Silently ignored | Rejected; unknown long option |

These are extensions absent from Apache: `-h`, `-V`, all long options, `-z`,
and `--retain`.

## Performance Benchmarks

```sh
cargo bench --bench writer
scripts/bench.sh          # requires hyperfine
scripts/bench-vs-apache.sh # compare against Apache rotatelogs
```

- Criterion: `write` at 64 B/1 KiB/64 KiB, `-l`, size rotation at 64 KiB/1 MiB/64 MiB.
- End-to-end: plain/echo/rotation/compression/local-time.
- Apache comparison results and methodology: `BENCHMARKS.md`.
- Baselines and interpretation: `PERFORMANCE.md`.
- CI compiles benchmarks with `cargo bench --no-run`.

## Tests

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

- 80 unit tests: CLI parsing, time alignment, strftime validation, name
  collisions, `-n`/`-t`/`-T`, links, gzip, retention, DST boundaries, compression
  levels, fsync policies, and the worker pool.
- 24 integration tests on Linux (23 on macOS): feed stdin into the real binary
  and check file creation, size/time rotation, `-e`, `-L`, `-p`, `-D`, `-f`,
  `-c`, `-z`/`--retain`, invalid settings, and graceful SIGTERM shutdown.
- 5 original CLI compatibility tests: compare effective settings, exit codes,
  and file behavior against a real Apache `rotatelogs` binary.

## Differences and Limitations Compared with Apache

- Option letters, positional argument order, time alignment, and the meaning
  of `-n`/`-t`/`-T`/`-c`/`-p` follow Apache. As in Apache, `-t` takes
  precedence over `-T`/`-n`.
- As with POSIX getopt, options precede the logfile; `--` ends option parsing.
- Invalid numbers, lowercase size units, out-of-range UTC offsets, and unknown
  long options produce explicit errors instead of Apache's `atoi()`/ignore behavior.
- Sizes over 4 GiB and large intervals/counts are calculated using 64-bit values.
  Apache may overflow its 32-bit `unsigned int`/`atoi()` conversions.
- `-h`, `-V`, long options, `-z`, and `--retain` are extensions.
- `-l` uses DST-aware local civil-time boundaries, so rotation times can differ
  from Apache by one hour on DST transition days.
- Name collisions preserve files with `.1`, `.2` suffixes; Apache may overwrite.
- Rotation failures keep writing to the previous file; Apache truncates it and
  records lost messages.
- `-L` prefers hard links and falls back to symbolic links if needed.
- `--retain` tracks **only files rotated by the current process**. Older
  archives from before a restart are not automatically cleaned up. Use an
  external tool such as logrotate for long-term retention management.
- `strftime` patterns must be UTF-8; paths without `%` may be non-UTF-8.
- Month/weekday names and `%c`/`%x`/`%X` use fixed English formatting,
  independent of the system locale.
- Unix-style environments (httpd pipes, `tail -F`, hard links) are the primary target.
- `-p` does not use a shell. Use a wrapper for pipelines or multiple arguments.
- A stdout write error with `-e`, including EPIPE, disables echo only; file
  logging continues.
- Relative paths are resolved against the process's working directory
  (`ServerRoot` when launched by httpd). Symlinks in the logfile path itself
  are not canonicalized.

## Exit Codes

| Code | Meaning |
| --- | --- |
| 0 | Clean shutdown (stdin EOF or SIGTERM/SIGINT) |
| 1 | Invalid command line or configuration |
| 2 | Runtime or I/O error |

## License

MIT License. See `LICENSE` for details.

Behavior and CLI semantics were based on Apache httpd's `support/rotatelogs.c`,
but this is an independent implementation containing none of the original C
source. See `CREDITS.md` for acknowledgments.
