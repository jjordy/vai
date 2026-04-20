//! `vai login` — CLI authentication command (PRD 26 V-4).
//!
//! Opens the user's browser to the dashboard `/cli-auth` page and waits for
//! the API key to be delivered on an ephemeral localhost port.  Falls back
//! automatically to the device code flow for headless environments.

use colored::Colorize;
use serde::Deserialize;

use crate::credentials::{self, Credentials};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from the `vai login` command.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("credentials error: {0}")]
    Credentials(#[from] credentials::CredentialsError),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

// ── Response types ────────────────────────────────────────────────────────────

/// JSON body posted by the dashboard to the localhost callback endpoint.
#[derive(Debug, Deserialize)]
struct CallbackBody {
    api_key: String,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    user_email: Option<String>,
}

/// Response from `POST /api/auth/cli-device`.
#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    code: String,
    verification_url: String,
    #[allow(dead_code)]
    poll_interval: u32,
}

/// Response from `GET /api/auth/cli-device/:code`.
#[derive(Debug, Deserialize)]
struct DeviceCodeStatus {
    status: String,
    api_key: Option<String>,
    user_id: Option<String>,
    user_email: Option<String>,
}

// ── Helper functions ──────────────────────────────────────────────────────────

/// Returns `true` when the login should use the device code flow instead of
/// the browser callback flow.
///
/// Auto-uses device mode when:
/// - `--device` flag is passed, OR
/// - No `DISPLAY` / `WAYLAND_DISPLAY` env var is set on Linux.
fn should_use_device_code(force_device: bool) -> bool {
    if force_device {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let has_display = std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some();
        if !has_display {
            return true;
        }
    }
    false
}

/// Returns the local hostname, reading `/etc/hostname` on Linux or falling
/// back to the `HOSTNAME` env var and finally `"unknown"`.
fn hostname() -> String {
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim().to_string();
        if !h.is_empty() {
            return h;
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string())
}

/// Percent-encodes a string for use in a URL query parameter value.
///
/// Only encodes characters that are not unreserved (letters, digits, `-`, `_`,
/// `.`, `~`).  Spaces become `%20`.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

// ── Port binding ──────────────────────────────────────────────────────────────

/// Binds a TCP listener on `127.0.0.1` within the PRD V-4 required range
/// [49152, 65535], randomizing the start offset to avoid thundering-herd
/// collisions when multiple `vai login` processes run concurrently.
///
/// Falls back to OS-assigned `:0` only if the entire sampled window (32 tries)
/// is busy — extremely rare in practice.
async fn bind_ephemeral_port() -> std::io::Result<tokio::net::TcpListener> {
    const LOW: u16 = 49152;
    const HIGH: u16 = 65535;
    const RANGE: u32 = (HIGH - LOW + 1) as u32;

    // Lightweight seed: subsecond nanoseconds of the current wall clock.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let start_offset = (seed % RANGE) as u16;

    for i in 0..32u16 {
        let port = LOW + ((u32::from(start_offset) + u32::from(i)) % RANGE) as u16;
        if let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            return Ok(listener);
        }
    }

    // Fallback: let the OS pick any free port (may be outside the required
    // range, but beats failing entirely on a saturated machine).
    tokio::net::TcpListener::bind("127.0.0.1:0").await
}

// ── Browser callback flow ─────────────────────────────────────────────────────

/// Waits for one POST to `http://127.0.0.1:<port>/callback` on `listener`,
/// validates the `state` query parameter, parses the JSON body, and sends an
/// HTTP 200 response.
async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<CallbackBody, LoginError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut stream, _) = listener.accept().await?;

    // Read the HTTP request into a buffer (8 KiB is plenty for our payload).
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Send HTTP 200 OK so the dashboard page can show a success message.
    let response =
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
    stream.write_all(response.as_bytes()).await?;
    drop(stream);

    // Validate the state query parameter from the request line.
    let request_line = request.lines().next().unwrap_or("");
    let state_token = format!("state={}", expected_state);
    if !request_line.contains(&state_token) {
        return Err(LoginError::Other(
            "state parameter mismatch in callback — possible CSRF; aborting".to_string(),
        ));
    }

    // Extract the JSON body (everything after the blank line).
    let body_str = request
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim();

    let body: CallbackBody = serde_json::from_str(body_str)
        .map_err(|e| LoginError::Other(format!("invalid callback body: {e}")))?;

    Ok(body)
}

/// Runs the browser-based auth flow.
///
/// 1. Binds a port in [49152, 65535] on `127.0.0.1` (PRD V-4).
/// 2. Opens the browser to `$dashboard_url/cli-auth?port=…&state=…&hostname=…&name=…`.
/// 3. Waits (up to 5 minutes) for the dashboard to POST back `{api_key, user_id, user_email}`.
/// 4. Writes credentials and returns.
async fn run_browser_flow(
    server_url: &str,
    dashboard_url: &str,
    key_name: &str,
) -> Result<Credentials, LoginError> {
    // Bind a port in the PRD V-4 required range [49152, 65535].
    let listener = bind_ephemeral_port().await?;
    let port = listener.local_addr()?.port();

    // 32-byte random state using UUID v4 (128 bits of entropy, hex-encoded).
    let state = uuid::Uuid::new_v4().simple().to_string();
    let host = hostname();

    let auth_url = format!(
        "{}/cli-auth?port={}&state={}&hostname={}&name={}",
        dashboard_url,
        port,
        state,
        percent_encode(&host),
        percent_encode(key_name),
    );

    println!("{}", "Opening browser for authentication…".bold());
    println!("  URL: {}", auth_url.cyan());
    println!();
    println!("If the browser does not open automatically, navigate to the URL above.");
    println!("Waiting up to 5 minutes for authorization…");
    println!("Tip: pass {} for headless environments.", "--device".yellow());
    println!();

    // Attempt to open the browser; fall back gracefully if it fails.
    if webbrowser::open(&auth_url).is_err() {
        println!("{}", "Could not open browser automatically.".yellow());
    }

    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(5 * 60),
        wait_for_callback(listener, &state),
    )
    .await;

    match timeout_result {
        Ok(Ok(body)) => {
            let creds = Credentials {
                server_url: server_url.to_string(),
                api_key: body.api_key,
                user_id: body.user_id,
                user_email: body.user_email,
            };
            credentials::write(&creds)?;
            Ok(creds)
        }
        Ok(Err(e)) => Err(e),
        Err(_elapsed) => Err(LoginError::Other(
            "timed out waiting for browser authorization. \
             Run `vai login --device` for headless environments."
                .to_string(),
        )),
    }
}

// ── Device code flow ──────────────────────────────────────────────────────────

/// Runs the device code auth flow.
///
/// 1. Calls `POST /api/auth/cli-device` to obtain a short code.
/// 2. Prints the code and verification URL.
/// 3. Polls `GET /api/auth/cli-device/:code` every 3 seconds until authorized
///    or expired (10-minute overall timeout).
/// 4. Writes credentials and returns.
async fn run_device_flow(server_url: &str) -> Result<Credentials, LoginError> {
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/auth/cli-device", server_url))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(LoginError::Other(format!("server error {status}: {body}")));
    }

    let device: DeviceCodeResponse = resp.json().await?;

    println!(
        "Visit {} and enter code: {}",
        device.verification_url.cyan(),
        device.code.bold().green()
    );
    println!("Waiting for authorization (polling every 3 s)…");

    let poll_url = format!("{}/api/auth/cli-device/{}", server_url, device.code);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10 * 60);

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        if std::time::Instant::now() > deadline {
            return Err(LoginError::Other(
                "timed out waiting for device code authorization".to_string(),
            ));
        }

        let poll = client.get(&poll_url).send().await?;
        let http_status = poll.status().as_u16();

        if http_status == 404 {
            return Err(LoginError::Other(
                "device code expired — run `vai login` again".to_string(),
            ));
        }

        let status: DeviceCodeStatus = poll.json().await?;
        match status.status.as_str() {
            "authorized" => {
                let api_key = status.api_key.ok_or_else(|| {
                    LoginError::Other(
                        "server returned authorized status but no api_key".to_string(),
                    )
                })?;
                let creds = Credentials {
                    server_url: server_url.to_string(),
                    api_key,
                    user_id: status.user_id,
                    user_email: status.user_email,
                };
                credentials::write(&creds)?;
                return Ok(creds);
            }
            "expired" => {
                return Err(LoginError::Other(
                    "device code expired — run `vai login` again".to_string(),
                ));
            }
            _ => {
                // "pending" — print a dot so the user knows we're alive.
                print!(".");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Handles the `vai login` command.
///
/// Selects browser or device code flow, writes credentials on success, and
/// prints a summary to stdout.
pub(super) fn handle(
    server_url: Option<String>,
    dashboard_url: Option<String>,
    device: bool,
    name: Option<String>,
) -> Result<(), super::CliError> {
    let server_url = server_url
        .or_else(|| std::env::var("VAI_SERVER_URL").ok())
        .unwrap_or_else(|| "https://vai.example.com".to_string());

    let dashboard_url = dashboard_url
        .or_else(|| std::env::var("VAI_DASHBOARD_URL").ok())
        .unwrap_or_else(|| server_url.clone());

    let host = hostname();
    let key_name = name.unwrap_or_else(|| format!("CLI on {host}"));

    let use_device = should_use_device_code(device);

    let rt = super::make_rt()?;

    let creds: Credentials = if use_device {
        rt.block_on(run_device_flow(&server_url))
            .map_err(|e| super::CliError::Other(e.to_string()))?
    } else {
        // Try browser flow; auto-fall back to device mode when browser can't open.
        match rt.block_on(run_browser_flow(&server_url, &dashboard_url, &key_name)) {
            Ok(c) => c,
            Err(LoginError::Other(ref msg))
                if msg.contains("Could not open browser") || msg.contains("timed out") =>
            {
                println!("Falling back to device code flow…");
                rt.block_on(run_device_flow(&server_url))
                    .map_err(|e| super::CliError::Other(e.to_string()))?
            }
            Err(e) => return Err(super::CliError::Other(e.to_string())),
        }
    };

    println!();
    println!("{}", "Logged in successfully!".green().bold());
    println!("  Server : {}", creds.server_url);
    if let Some(email) = &creds.user_email {
        println!("  User   : {email}");
    }
    let key_display_len = creds.api_key.len().min(12);
    println!("  Key    : {}…", &creds.api_key[..key_display_len]);
    if let Some(path) = credentials::credentials_path() {
        println!("  Saved  : {}", path.display());
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_spaces_and_specials() {
        assert_eq!(percent_encode("hello world"), "hello%20world");
        assert_eq!(percent_encode("CLI on my-host"), "CLI%20on%20my-host");
        assert_eq!(percent_encode("safe~chars-._"), "safe~chars-._");
    }

    #[test]
    fn should_use_device_code_when_forced() {
        assert!(should_use_device_code(true));
    }

    #[test]
    fn should_not_use_device_code_by_default_on_non_linux() {
        // On non-Linux platforms we never auto-detect headless, so the flag
        // should be false when --device is not passed (assuming the env is
        // not deliberately set to trigger the Linux branch).
        #[cfg(not(target_os = "linux"))]
        assert!(!should_use_device_code(false));
    }

    /// PRD V-4: the ephemeral port binder must stay within [49152, 65535].
    #[tokio::test]
    async fn bind_ephemeral_port_is_in_required_range() {
        let listener = bind_ephemeral_port().await.expect("bind failed");
        let port = listener.local_addr().expect("local_addr").port();
        // The `:0` fallback may fire on a very busy test machine; we accept that
        // but the primary path must be in range.
        if port != 0 {
            assert!(
                (49152..=65535).contains(&port),
                "port {port} is outside required range [49152, 65535]"
            );
        }
    }
}
