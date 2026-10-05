# STARTHERE.md - AI Bootstrap Guide

> This file is designed for AI coding assistants. If you are a human,
> see [README.md](README.md) for the human-friendly guide.

## Quick Bootstrap

```bash
git clone https://github.com/GreyforgeLabs/tongs.git && cd tongs && ./scripts/setup.sh
```

## What This Project Does

tongs (formerly atomic-json-store) provides atomic, cross-process locked, schema-versioned JSON persistence as a Rust library and CLI. One type (`tongs::Store`) writes documents through temp-file-plus-rename, serializes read-modify-write cycles with an advisory sidecar `flock`, and upgrades old files through registered migrations. Version 2.x is a Rust rewrite of the Python 1.x package, which was released as `atomic-json-store`; the file format (envelope marker `"atomic-json-store/1"`, which must not change), JSON formatting and lock protocol are unchanged, so Rust and Python processes can share a store. There is no Python package named tongs.

The `atomic-json-store` binary (`src/bin/atomic-json-store.rs`) is a deprecated alias kept for one release: it prints a one-line note to stderr and then runs the same CLI.

`info()` reads a complete atomically published file without creating a directory or lock. CLI dotted key paths have no escape syntax; use the library API for keys containing a literal dot.

## Project Structure

```text
tongs/
  src/
    lib.rs              # crate docs and public exports
    store.rs            # Store, builder, envelope, migrations, locking
    json.rs             # CPython-compatible json.loads / json.dumps
    python.rs           # repr(float), repr(str), pathlib normalisation, float()/int()
    error.rs            # Error / ErrorKind (Python-identical messages)
    sys.rs              # flock and signal deferral (libc), mkdir -p, mkstemp, dir fsync, UTC time
    main.rs             # `tongs` binary entry point
    bin/
      atomic-json-store.rs  # deprecated alias binary (one release)
    cli/
      mod.rs            # get/set/delete/dump/info/init
      argparse.rs       # faithful argparse emulation (parsing, help, errors)
      keypath.rs        # dotted key paths
  tests/
    store.rs            # library behaviour (ported from test_core.py)
    cli.rs              # CLI behaviour and exit codes (ported from test_cli.py)
    differential.rs     # opt-in parity/interop checks against Python 1.x
  docs/
    benchmarks.md       # Python 1.x vs Rust 2.x measurements
    test-mapping.md     # 1.x pytest -> Rust test mapping
  scripts/
    setup.sh            # idempotent build + verification
  .github/workflows/    # CI (Linux + macOS) and tagged binary release
  README.md             # human-facing docs
  STARTHERE.md          # this file
```

## Setup Prerequisites

- Rust 1.88 or newer (`rustup`), with `rustfmt` and `clippy` for development
- Linux or macOS
- No system dependencies

## Installation Steps

1. Clone: `git clone https://github.com/GreyforgeLabs/tongs.git`
2. Enter directory: `cd tongs`
3. Run setup: `./scripts/setup.sh`
4. Optional: install the CLI with `cargo install --locked --path .`

## Verification

```bash
cargo run --quiet -- --version
# Expected output: tongs 2.0.0
cargo test --locked
# Expected output: all tests pass
```

## Key Entry Points

- `src/store.rs` - `Store` (`load`, `save`, `update`, `transaction`, `get`, `set`, `reset`, `info`, `lock`, `save_as`, `load_as`)
- `src/cli/mod.rs` - `tongs` command

## Configuration

No configuration files or environment variables (the CLI honours `COLUMNS` for help-text width, like argparse). All behaviour is set through `StoreBuilder` methods (`schema_version`, `migration`, `default_value`/`default_with`, `lock_timeout`, `on_corrupt`, `fsync`, `file_mode`, `indent`, `sort_keys`, `ensure_ascii`) or CLI flags (`--lock-timeout`, `--schema-version`, `--json`, `--default`).

## Common Tasks

```bash
# Run tests
cargo test --locked

# Parity and interoperability checks against the Python 1.x package
# (atomic-json-store v1.0.1; CLI output is compared with the program name normalised)
git worktree add --detach ../ajs-v1 v1.0.1
AJS_PYTHON_REF=../ajs-v1/src cargo test --locked --test differential

# Lint and format
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings

# Release build
cargo build --release --locked
```
