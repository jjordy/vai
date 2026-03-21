//! vai — version control for AI agents
//!
//! This is the CLI entry point. All commands are dispatched through the `cli` module.

use clap::Parser;
use vai::cli::{Cli, execute};

fn main() {
    let cli = Cli::parse();
    if let Err(e) = execute(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
