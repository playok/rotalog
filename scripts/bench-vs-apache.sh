#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/rotalog}"
ORACLE="${ROTATELOGS_ORACLE:-/usr/sbin/rotatelogs}"
BYTES="${BENCH_BYTES:-33554432}"
ROTATE_SIZE="${ROTATE_SIZE:-8M}"
RUNS="${BENCH_RUNS:-20}"
WARMUP="${BENCH_WARMUP:-5}"

if [[ ! -x "$BIN" ]]; then
    echo "building release binary..."
    (cd "$ROOT" && cargo build --release)
fi

if [[ ! -x "$ORACLE" ]]; then
    echo "original rotatelogs not found at $ORACLE" >&2
    echo "set ROTATELOGS_ORACLE to the Apache rotatelogs binary" >&2
    exit 1
fi

if ! command -v hyperfine >/dev/null 2>&1; then
    echo "hyperfine is required: cargo install hyperfine (or brew install hyperfine)" >&2
    exit 1
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/rotalog-vs-apache.XXXXXX")"
out="$ROOT/target/bench-vs-apache"
mkdir -p "$out" "$tmp/rotalog" "$tmp/rotatelogs"
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

echo "rotalog:  $BIN"
echo "original: $ORACLE"
echo "fixture:  $BYTES bytes, rotation limit $ROTATE_SIZE"
echo

hyperfine \
    --shell=default \
    --warmup "$WARMUP" \
    --runs "$RUNS" \
    --prepare "rm -f '$tmp/rotalog/'*.log* '$tmp/rotatelogs/'*.log*" \
    --command-name "rotalog/plain" \
    "$BIN '$tmp/rotalog/plain.log' 86400 < '$tmp/fixture.bin'" \
    --command-name "rotatelogs/plain" \
    "$ORACLE '$tmp/rotatelogs/plain.log' 86400 < '$tmp/fixture.bin'" \
    --command-name "rotalog/echo" \
    "$BIN -e '$tmp/rotalog/echo.log' 86400 < '$tmp/fixture.bin' > /dev/null" \
    --command-name "rotatelogs/echo" \
    "$ORACLE -e '$tmp/rotatelogs/echo.log' 86400 < '$tmp/fixture.bin' > /dev/null" \
    --command-name "rotalog/rotate-$ROTATE_SIZE" \
    "$BIN '$tmp/rotalog/rotate.log' $ROTATE_SIZE < '$tmp/fixture.bin'" \
    --command-name "rotatelogs/rotate-$ROTATE_SIZE" \
    "$ORACLE '$tmp/rotatelogs/rotate.log' $ROTATE_SIZE < '$tmp/fixture.bin'" \
    --command-name "rotalog/local" \
    "$BIN -l '$tmp/rotalog/local.log' 86400 < '$tmp/fixture.bin'" \
    --command-name "rotatelogs/local" \
    "$ORACLE -l '$tmp/rotatelogs/local.log' 86400 < '$tmp/fixture.bin'" \
    --export-json "$out/results.json" \
    --export-markdown "$out/results.md"

echo
cat "$out/results.md"
echo
echo "results: $out/results.json"
echo "note: compression (-z) is intentionally omitted; Apache rotatelogs has no -z."
