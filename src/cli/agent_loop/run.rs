//! Stub for `vai agent loop run`.

use crate::cli::CliError;

/// Handle `vai agent loop run` — not yet implemented.
pub(super) fn handle(_name: Option<&str>, _json: bool) -> Result<(), CliError> {
    eprintln!("Loop run not yet implemented (tracking issue: #279 V-7 → V-9).");
    Ok(())
}
