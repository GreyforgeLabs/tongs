//! Deprecated `atomic-json-store` alias for `tongs`.
//!
//! The command was renamed from atomic-json-store to tongs in 2.0.0. This
//! alias prints a one-line deprecation note to stderr and then behaves exactly
//! like `tongs` (same arguments, output, files and exit codes). It will be
//! removed in the next release.

use std::io::Write;

#[path = "../cli/mod.rs"]
mod cli;

/// The deprecation note printed to stderr before every invocation.
const DEPRECATION_NOTE: &str =
    "atomic-json-store: renamed to tongs; this alias will be removed in the next release";

fn main() {
    // Never fail because stderr is closed or a broken pipe: the alias must
    // behave exactly like `tongs` apart from this note.
    let _ = writeln!(std::io::stderr(), "{DEPRECATION_NOTE}");
    let code = cli::main(std::env::args_os().skip(1).collect());
    std::process::exit(code);
}
