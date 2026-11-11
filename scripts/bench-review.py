#!/usr/bin/env python3
"""Compare release binaries and Apache with identical inputs; requires hyperfine 2."""
import argparse
import json
import pathlib
import random
import shlex
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", action="append", required=True, metavar="NAME=PATH")
parser.add_argument("--bytes", type=int, default=32 * 1024 * 1024)
parser.add_argument("--runs", type=int, default=20)
parser.add_argument("--output", type=pathlib.Path, default=pathlib.Path("target/perf-review"))
args = parser.parse_args()
if args.bytes < 1 or args.runs < 1:
    parser.error("--bytes and --runs must be positive")
binaries = []
for item in args.binary:
    name, path = item.split("=", 1)
    path = pathlib.Path(path).resolve(strict=True)
    binaries.append((name, path))
args.output.mkdir(parents=True, exist_ok=True)
rows = ["| Workload | Binary | Wall median ms | CPU mean ms | Peak RSS mean MiB |",
        "|---|---|---:|---:|---:|"]
with tempfile.TemporaryDirectory(prefix="rotalog-review-") as temporary:
    root = pathlib.Path(temporary)
    fixture = root / "input"
    line = b'127.0.0.1 - - [11/Nov/2026:20:30:00 +0900] "GET / HTTP/1.1" 200 1234\n'
    block = line * 1024
    with fixture.open("wb") as stream:
        remaining = args.bytes
        while remaining:
            chunk = block[:min(remaining, len(block))]
            stream.write(chunk)
            remaining -= len(chunk)
    quote = shlex.quote
    for mode in ["file", "echo", "rotate", "local", "pipe"]:
        ordered = list(enumerate(binaries))
        random.Random(42 + len(rows)).shuffle(ordered)
        command = ["hyperfine", "--shell=default", "--warmup", "3", "--runs", str(args.runs),
                   "--metrics", "all", "--export-json", str(args.output / f"{mode}.json")]
        for index, (name, binary) in ordered:
            directory = root / str(index)
            directory.mkdir(exist_ok=True)
            flags = {"echo": "-e", "local": "-l"}.get(mode, "")
            limit = "8M" if mode == "rotate" else "86400"
            run = f"{quote(str(binary))} {flags} {quote(str(directory / 'out.log'))} {limit}"
            if mode == "pipe":
                run = f"cat {quote(str(fixture))} | {run}"
            else:
                run += f" < {quote(str(fixture))}"
            run += " > /dev/null"
            command += ["--command-name", name, run]
        command += ["--prepare", f"find {quote(str(root))} -name 'out.log*' -delete"]
        subprocess.run(command, check=True)
        result = json.loads((args.output / f"{mode}.json").read_text())
        for item in result["results"]:
            summary = item["summary"]
            wall = summary["time_wall_clock"]["median"] * 1000
            cpu = summary["time_cpu"]["mean"] * 1000
            rss = summary["memory_peak_resident"]["mean"] / 1048576
            rows.append(f"| {mode} | {item['name']} | {wall:.2f} | {cpu:.2f} | {rss:.2f} |")
(args.output / "summary.md").write_text("\n".join(rows) + "\n")
print("Pipe CPU includes cat; RSS is the hyperfine process-tree peak metric.")
print(args.output / "summary.md")
