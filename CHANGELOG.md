# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [2.0.0] - 2026-10-05

### Changed

- Renamed from atomic-json-store to tongs. The crate, library and binary are now `tongs` and the repository is `GreyforgeLabs/tongs`. The previous command name keeps working for this release as a deprecated `atomic-json-store` binary that prints a one-line note to stderr and then behaves exactly like `tongs`; it will be removed in the next release. The on-disk envelope marker deliberately stays `"format": "atomic-json-store/1"`, because Python 1.x accepts only that exact string, and tongs requires it on read.
- Rewritten in Rust. The Python package is replaced by a Rust library crate (`tongs`, main type `tongs::Store`) and the `tongs` binary. This is a major release because the Python API is gone, and there is no Python package named tongs. Python programs can keep using 1.0.x, which stays available from the `v1.0.1` tag (`pip install "git+https://github.com/GreyforgeLabs/tongs@v1.0.1"`); that tag still installs the package `atomic-json-store` and imports as `atomic_json_store`.
- Files and locking are unchanged and interoperable in both directions: the same envelope keys and order (including the `atomic-json-store/1` format marker), byte-identical JSON formatting (indentation, separators, key order, `ensure_ascii` handling, CPython float `repr`, `NaN`/`Infinity`, arbitrary-size integers), the same `<file>.lock` sidecar with the same `flock` semantics, `0600` for new files and preserved modes. Rust and Python processes can update one store concurrently without losing writes (covered by `tests/differential.rs`).
- The CLI keeps the same subcommands, flags, defaults, help text, error messages, JSON output and exit codes apart from the program name (`tongs`); argument parsing reproduces Python's `argparse` (abbreviations, `--opt=value`, negative-number values, `--`, terminal-width help wrapping).
- Library API: `Store::builder(path)` (the Python class was `AtomicJsonStore`) with `schema_version`, `migration(from, closure)`, `default_value` / `default_with`, `lock_timeout`, `indent`, `sort_keys`, `ensure_ascii`, `fsync`, `on_corrupt`, `file_mode`; operations `load`, `save`, `update`, `transaction` (closure, commits on `Ok`), `get` / `get_or`, `set`, `reset`, `info`, `exists`, `lock` (public re-entrant lock guard), and serde-typed `save_as` / `load_as` in place of custom encoder/decoder classes. Errors carry an `ErrorKind` and the same message text as the Python exceptions.
- Packaging and tooling: Cargo replaces setuptools; CI runs `cargo fmt`, `clippy -D warnings` and `cargo test` on Linux and macOS, plus a build and test job on the minimum supported Rust version (1.88); tagged releases build Linux x86_64 and macOS arm64 archives containing `tongs` and the `atomic-json-store` alias; the PyPI publish workflow is removed.
- Interrupting a write is safe: from creating the temporary file until the rename and directory fsync are done, the writing thread holds back `SIGHUP`, `SIGINT`, `SIGQUIT` and `SIGTERM`, which are then delivered as usual. Ctrl-C or a plain `kill` (`SIGTERM`) during a CLI write never leaves a `.<name>.XXXXXXXX.tmp` file behind (1.0.1 cleaned up on Ctrl-C through `KeyboardInterrupt`) and the store always holds either the old or the new document.

### Performance

Measured on Linux x86_64 (see `docs/benchmarks.md`): cold start of `--version` 57.4 ms -> 2.4 ms, `get` 56.4 ms -> 2.0 ms, `set` with fsync 59.2 ms -> 5.1 ms; peak RSS about 17.5 MB -> 2.5 MB; a 579 KB stripped binary with no interpreter dependency instead of 23.5 KB of source plus a CPython 3.11+ runtime.

### Intentional deviations from 1.0.1

- Failures that 1.0.1 left as uncaught Python tracebacks now print one line, `tongs: <message>`, with the same exit status 1: a negative `--lock-timeout` (`lock_timeout must be >= 0 or None`), a store path with an empty final component such as `.` or `/` (`PosixPath('.') has an empty name`), non-UTF-8 key or value arguments to `set`, an undecodable string given to `get --default`, and `info`/`init` on a store path that is not valid UTF-8 (CPython's strict-UTF-8 stdout failed; the message is the same `'utf-8' codec can't encode character ...`). Elsewhere, bytes of non-UTF-8 arguments are shown as `\udcXX`, exactly as 1.0.1 printed them.
- Help and usage text is never coloured. Python 3.14's `argparse` colours help when stdout is a terminal; the uncoloured text is identical.
- Unix-like systems (Linux, macOS) only. The untested best-effort Windows (`msvcrt`) locking path is not carried over.
- JSON strings containing unpaired UTF-16 surrogates (an escape such as `"\ud800"`, or a surrogate encoded directly in UTF-8, UTF-16 or UTF-32 bytes) are rejected as invalid JSON, because a Rust `String` cannot hold them; 1.0.1 accepted them on read (but could not write them with the default `ensure_ascii=False`). A store containing one is reported as corrupt (and moved aside under the quarantine policy).
- Documents nested deeper than 4096 levels are refused as invalid JSON (reported as corrupt), and a write whose envelope would exceed that depth fails with `ErrorKind::Serialize` before anything is written, so a crafted file cannot crash the process with a stack overflow. 1.0.1 raised an uncaught `RecursionError` at a depth that depended on the CPython version (about 1000 levels on 3.11, about 100 000 on 3.14). `load_as` refuses documents nested more than 128 levels deep, as `serde_json::from_str` does.
- Integers longer than 4300 digits are accepted; CPython's integer string-conversion limit made 1.0.1 fail on them.
- Schema versions above 18446744073709551615 are treated as invalid, and `init --schema-version` rejects such values.
- `LockTimeout` messages always print the timeout as a float (`5.0s`); 1.0.1 printed a Python `int` timeout passed through the library API as `5s`. CLI output is unchanged.
- A migration closure that returns JSON `null` is rejected like a Python migration returning `None` (the two were the same value in Python).
- A `SIGINT` that arrives while a write is being published no longer aborts it: 1.0.1 raised `KeyboardInterrupt`, removed its temporary file and kept the old document; 2.0.0 finishes the publish and then lets the signal take effect, so the store holds the new document. In both, no temporary file remains. 1.0.1 left its temporary file behind on `SIGTERM` or `SIGHUP`; 2.0.0 does not.

## [1.0.1] - 2026-09-27

### Fixed

- `info()` now reads an atomically published generation without creating a parent directory or sidecar lock.
- Document the CLI's dotted-path key limitation and the direct Python API for literal dots.

## [1.0.0] - 2026-09-06

### Added

- `AtomicJsonStore` with atomic temp-file-plus-`os.replace()` writes, fsync of file and directory, and private `0600` default file mode
- Cross-process advisory locking on a sidecar `<file>.lock` (shared for reads, exclusive for writes), re-entrant per thread, with configurable timeout
- Schema-versioned envelope (`format`, `schema_version`, `updated_at`, `data`) with ordered migrations, refusal of newer files, and version-0 adoption of plain JSON files
- `load`, `save`, `update`, `transaction`, `get`, `set`, `reset`, and `info`
- Corrupt-file policy: `raise` (default) or `quarantine`
- `atomic-json-store` CLI with `init`, `info`, `dump`, `get`, `set`, `delete`, dotted key paths, and distinct exit codes
- Test suite covering atomicity under failed replace, unserializable input, thread and multi-process increment counts, concurrent reader consistency, lock timeouts, migrations, and corruption handling
- GitHub Actions CI on Linux and macOS for Python 3.11, 3.12, and 3.13; tagged release workflow; manual PyPI publish workflow
- README, STARTHERE bootstrap, and idempotent setup script
