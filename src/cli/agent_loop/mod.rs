//! Agent loop subcommand handlers.
//!
//! Entry point for `vai agent loop <SUBCOMMAND>`.
//! All subcommands are currently stubs; real logic will be filled in by
//! subsequent issues (V-8 through V-14).

mod init;
mod list;
mod run;

use crate::cli::{CliError, LoopCommands};

/// Dispatch `vai agent loop` subcommands.
pub(crate) fn handle_loop(cmd: LoopCommands, json: bool) -> Result<(), CliError> {
    match cmd {
        LoopCommands::Init {
            agent,
            project_type,
            docker,
            overwrite,
            name,
        } => init::handle(
            agent.as_deref(),
            project_type.as_deref(),
            docker,
            overwrite,
            name.as_deref(),
            json,
        ),
        LoopCommands::Run { name } => run::handle(name.as_deref(), json),
        LoopCommands::List => list::handle(json),
    }
}
