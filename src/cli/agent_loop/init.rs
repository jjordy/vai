//! Stub for `vai agent loop init`.

use crate::cli::CliError;

/// Handle `vai agent loop init` — not yet implemented.
pub(super) fn handle(
    _agent: Option<&str>,
    _project_type: Option<&str>,
    _docker: bool,
    _overwrite: bool,
    _name: Option<&str>,
    _json: bool,
) -> Result<(), CliError> {
    eprintln!("Loop generation not yet implemented (tracking issue: #279 V-7 → V-8).");
    Ok(())
}
