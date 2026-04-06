//! Dashboard handler.

use crate::repo;
use super::CliError;

/// Handle `vai dashboard`.
pub(super) fn handle(server: Option<String>, key: Option<String>) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");
    if let Some(server_url) = server {
        #[cfg(feature = "server")]
        {
            let api_key = key.unwrap_or_default();
            crate::dashboard::run_server(&vai_dir, &server_url, &api_key)
                .map_err(|e| CliError::Other(e.to_string()))?;
        }
        #[cfg(not(feature = "server"))]
        {
            let _ = server_url;
            let _ = key;
            return Err(CliError::Other(
                "server dashboard requires the 'server' feature (rebuild with --features server)".to_string()
            ));
        }
    } else {
        crate::dashboard::run(&vai_dir)
            .map_err(|e| CliError::Other(e.to_string()))?;
    }
    Ok(())
}
