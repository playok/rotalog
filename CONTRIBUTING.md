# Contributing

Thanks for considering a contribution to `rotalog`.

## Development Setup

- Rust 1.85 or newer (edition 2024)
- Unix-like OS for the full feature set (signals, hard links, `tail -F`)

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## CLI Compatibility Tests

The compatibility suite compares `rotalog` with an original Apache
`rotatelogs` binary when one is available:

```sh
ROTATELOGS_ORACLE=/usr/sbin/rotatelogs \
  cargo test --test original_cli_compat -- --nocapture
```

Without `ROTATELOGS_ORACLE`, common install paths are tried, then the tests skip
with a message. On Ubuntu, `sudo apt-get install apache2-bin` provides the
oracle. Do not point `ROTATELOGS_ORACLE` at a `rotatelogs` symlink that resolves
to `rotalog`; the suite rejects it.

## Style and Review

- Keep rotation logic testable by passing the current time explicitly.
- Prefer clear error contexts over panics in runtime paths.
- Add or update tests for behavior changes.
- Update `README.md`, `README.en.md`, and `CHANGELOG.md` when user-visible
  behavior changes.
- Keep `#![forbid(unsafe_code)]` intact unless there is a compelling reason.

## Commit and Pull Request

- Use focused commits with a short imperative subject.
- Reference related issues with `Closes #N` or `Refs #N` in the PR body.
- Ensure CI (fmt, clippy, tests, MSRV, audit) is green before review.
- For security issues, follow `SECURITY.md` instead of opening a public PR or
  issue first.

## License

By contributing, you agree that your contribution is licensed under the MIT
License, the same license as this project.
