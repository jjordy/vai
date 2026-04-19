//! Agent loop subcommand handlers.
//!
//! Entry point for `vai agent loop <SUBCOMMAND>`.

pub mod detection;
pub mod env;
pub mod env_writer;
pub mod generate;
pub mod templates;
mod init;
mod list;
mod run;

#[allow(unused_imports)]
pub use detection::{detect_project_type, ProjectType};
#[allow(unused_imports)]
pub use templates::{template, TemplateKind};

use crate::cli::{CliError, LoopCommands};

/// Dispatch `vai agent loop` subcommands.
pub(crate) fn handle_loop(cmd: LoopCommands, json: bool) -> Result<(), CliError> {
    match cmd {
        LoopCommands::Init {
            agent,
            project_type,
            docker,
            no_docker,
            overwrite,
            name,
        } => init::handle(
            agent.as_deref(),
            project_type.as_deref(),
            docker,
            no_docker,
            overwrite,
            name.as_deref(),
            json,
        ),
        LoopCommands::Run { name } => run::handle(name.as_deref(), json),
        LoopCommands::List => list::handle(json),
    }
}
