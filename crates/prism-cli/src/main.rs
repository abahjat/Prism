// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism CLI Binary Entry Point
//!
//! Invokes [`prism_cli::run_from_iter`] and exits with the returned status code.

use std::process::ExitCode;

fn main() -> ExitCode {
    let outcome = prism_cli::run_from_iter(std::env::args_os());
    if !outcome.stdout.is_empty() {
        if outcome.exit_code == prism_cli::EXIT_SUCCESS {
            println!("{}", outcome.stdout);
        } else {
            eprintln!("{}", outcome.stdout);
        }
    }
    ExitCode::from(outcome.exit_code)
}
