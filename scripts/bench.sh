#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/rotalog}"
BYTES="${BENCH_BYTES:-134217728}"
RUNS="${BENCH_RUNS:-5}"
WARMUP="${BENCH_WARMUP:-1}"

if [[ ! -x "$BIN" ]]; then
    echo "building release binary..."
    (cd "$ROOT" && cargo build --release)
fi

if ! command -v hyperfine >/dev/null 2>&1; then
    echo "hyperfine is required: cargo install hyperfine (or brew install hyperfine)" >&2
    exit 1
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/rotalog-bench.XXXXXX")"
out="$ROOT/target/bench"
mkdir -p "$out"
trap 'rm -rf "$tmp"' EXIT

python3 - "$tmp/fixture.bin" "$BYTES" <<'PY'
import sys

path, size = sys.argv[1], int(sys.argv[2])
line = (
    b'127.0.0.1 - - [11/Nov/2026:20:30:00 +0900] '
    b'"GET /index.html HTTP/1.1" 200 1234 "-" "benchmark"\n'
)
block = line * 1024
with open(path, "wb") as fixture:
    remaining = size
    while remaining > 0:
        chunk = block[: min(len(block), remaining)]
        fixture.write(chunk)
        remaining -= len(chunk)
PY

echo "fixture: $tmp/fixture.bin ($BYTES bytes)"
echo "binary:  $BIN"
echo

hyperfine \
    --shell=default \
    --warmup "$WARMUP" \
    --runs "$RUNS" \
    --prepare "rm -f '$tmp'/*.log*" \
    --command-name "plain" \
    "$BIN '$tmp/plain.log' 86400 < '$tmp/fixture.bin'" \
    --command-name "echo" \
    "$BIN -e '$tmp/echo.log' 86400 < '$tmp/fixture.bin' > /dev/null" \
    --command-name "rotate-64KiB" \
    "$BIN '$tmp/rotate.log' 64K < '$tmp/fixture.bin'" \
    --command-name "compress-32MiB" \
    "$BIN -z '$tmp/compress.log' 32M < '$tmp/fixture.bin'" \
    --command-name "compress-32MiB-level1" \
    "$BIN -z --compress-level 1 '$tmp/compress1.log' 32M < '$tmp/fixture.bin'" \
    --command-name "compress-32MiB-level9" \
    "$BIN -z --compress-level 9 '$tmp/compress9.log' 32M < '$tmp/fixture.bin'" \
    --command-name "compress-32MiB-nofsync" \
    "$BIN -z --fsync-archives never '$tmp/compress-nofsync.log' 32M < '$tmp/fixture.bin'" \
    --command-name "compress-32MiB-jobs2" \
    "$BIN -z --compress-jobs 2 '$tmp/compress-jobs2.log' 32M < '$tmp/fixture.bin'" \
    --command-name "compress-32MiB-jobs4" \
    "$BIN -z --compress-jobs 4 '$tmp/compress-jobs4.log' 32M < '$tmp/fixture.bin'" \
    --command-name "local" \
    "$BIN -l '$tmp/local.log' 86400 < '$tmp/fixture.bin'" \
    --export-json "$out/results.json" \
    --export-markdown "$out/results.md"

echo
cat "$out/results.md"
echo
echo "results: $out/results.json"
