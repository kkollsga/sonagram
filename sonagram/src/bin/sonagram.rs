//! The `sonagram` binary — a thin shim over [`sonagram::cli::run`].
//!
//! All subcommand logic lives in the `sonagram` library (`src/cli.rs`) so the
//! cargo binary and the `pip install sonagram` console script share one code
//! path and cannot drift. This frontend only forwards argv and exits.

use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    exit(sonagram::cli::run(&args));
}
