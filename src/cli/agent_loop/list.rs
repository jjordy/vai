//! Stub for `vai agent loop list`.

use crate::cli::CliError;

/// Handle `vai agent loop list` — not yet implemented.
pub(super) fn handle(_json: bool) -> Result<(), CliError> {
    eprintln!("Loop list not yet implemented (tracking issue: #279 V-7 → V-10).");
    Ok(())
}
