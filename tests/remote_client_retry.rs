//! Integration tests for `RemoteClient` retry and backoff behaviour.
//!
//! These tests spin up a minimal in-process HTTP server using raw Tokio
//! TCP sockets to avoid pulling in a heavyweight test-server dependency.

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use vai::remote_client::RemoteClient;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Binds a TCP listener on a random local port and returns it together with
/// the port number.
async fn bind_local() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// Reads the HTTP request line from `stream` (discards headers/body).
async fn drain_request(stream: &mut tokio::net::TcpStream) {
    let mut buf = [0u8; 4096];
    // Read until we see the end of the HTTP headers (\r\n\r\n).
    let mut total = 0usize;
    loop {
        let n = stream.read(&mut buf[total..]).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        total += n;
        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if total >= buf.len() {
            break;
        }
    }
}

/// Sends a minimal HTTP response with the given status code and JSON body.
async fn send_response(stream: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        503 => "Service Unavailable",
        404 => "Not Found",
        _ => "Unknown",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len(),
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A server that returns 503 on the first GET, then 200 on the second.
/// Verifies that GET (idempotent) is retried on 5xx.
#[tokio::test]
async fn get_retries_on_5xx_and_eventually_succeeds() {
    let (listener, port) = bind_local().await;
    let attempt_count = Arc::new(AtomicU32::new(0));
    let count_clone = Arc::clone(&attempt_count);

    tokio::spawn(async move {
        for _ in 0..2u32 {
            let (mut stream, _) = listener.accept().await.unwrap();
            drain_request(&mut stream).await;
            let attempt = count_clone.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                send_response(&mut stream, 503, r#"{"error":"overloaded"}"#).await;
            } else {
                send_response(&mut stream, 200, r#"{"ok":true}"#).await;
            }
        }
    });

    let client = RemoteClient::new(&format!("http://127.0.0.1:{port}"), "test-key");
    let result: serde_json::Value = client.get("/api/test").await.expect("should succeed");
    assert_eq!(result["ok"], serde_json::Value::Bool(true));
}

/// A server that always returns 404.
/// Verifies that GET does NOT retry on 4xx.
#[tokio::test]
async fn get_does_not_retry_on_4xx() {
    let (listener, port) = bind_local().await;
    let attempt_count = Arc::new(AtomicU32::new(0));
    let count_clone = Arc::clone(&attempt_count);

    tokio::spawn(async move {
        // Serve up to 4 requests so we can detect unexpected retries.
        for _ in 0..4u32 {
            let accepted = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                listener.accept(),
            )
            .await;
            match accepted {
                Ok(Ok((mut stream, _))) => {
                    drain_request(&mut stream).await;
                    count_clone.fetch_add(1, Ordering::SeqCst);
                    send_response(&mut stream, 404, r#"{"error":"not found"}"#).await;
                }
                _ => break,
            }
        }
    });

    let client = RemoteClient::new(&format!("http://127.0.0.1:{port}"), "test-key");
    let err = client
        .get::<serde_json::Value>("/api/missing")
        .await
        .expect_err("should fail on 4xx");
    assert!(
        matches!(err, vai::remote_client::RemoteClientError::HttpError { status: 404, .. }),
        "expected HttpError 404, got {err:?}"
    );

    // Give the server task a moment to record any unexpected retries.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        attempt_count.load(Ordering::SeqCst),
        1,
        "should have made exactly 1 attempt on a 4xx response"
    );
}

/// A server that returns 503 on the first unsafe POST, then would return 200.
/// Verifies that `post` (ConnectOnly policy) does NOT retry on 5xx.
#[tokio::test]
async fn post_does_not_retry_on_5xx() {
    let (listener, port) = bind_local().await;
    let attempt_count = Arc::new(AtomicU32::new(0));
    let count_clone = Arc::clone(&attempt_count);

    tokio::spawn(async move {
        for _ in 0..4u32 {
            let accepted = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                listener.accept(),
            )
            .await;
            match accepted {
                Ok(Ok((mut stream, _))) => {
                    drain_request(&mut stream).await;
                    count_clone.fetch_add(1, Ordering::SeqCst);
                    send_response(&mut stream, 503, r#"{"error":"overloaded"}"#).await;
                }
                _ => break,
            }
        }
    });

    let client = RemoteClient::new(&format!("http://127.0.0.1:{port}"), "test-key");
    let err = client
        .post::<serde_json::Value, serde_json::Value>("/api/workspaces", &serde_json::json!({}))
        .await
        .expect_err("should fail on 5xx");
    assert!(
        matches!(err, vai::remote_client::RemoteClientError::HttpError { status: 503, .. }),
        "expected HttpError 503, got {err:?}"
    );

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        attempt_count.load(Ordering::SeqCst),
        1,
        "unsafe POST should not be retried on 5xx"
    );
}

/// A server that returns 503 on the first `post_idempotent` call, then 200.
/// Verifies that `post_idempotent` DOES retry on 5xx.
#[tokio::test]
async fn post_idempotent_retries_on_5xx() {
    let (listener, port) = bind_local().await;
    let attempt_count = Arc::new(AtomicU32::new(0));
    let count_clone = Arc::clone(&attempt_count);

    tokio::spawn(async move {
        for _ in 0..2u32 {
            let (mut stream, _) = listener.accept().await.unwrap();
            drain_request(&mut stream).await;
            let attempt = count_clone.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                send_response(&mut stream, 503, r#"{"error":"overloaded"}"#).await;
            } else {
                send_response(&mut stream, 200, r#"{"token":"abc"}"#).await;
            }
        }
    });

    let client = RemoteClient::new(&format!("http://127.0.0.1:{port}"), "test-key");
    let result: serde_json::Value = client
        .post_idempotent("/api/auth/token", &serde_json::json!({"key": "x"}))
        .await
        .expect("idempotent post should succeed after retry");
    assert_eq!(result["token"], "abc");
}

/// Connects to a port that has nothing listening (connection refused) on the
/// first attempt, then a server starts up and the retry succeeds.
///
/// This validates that connect errors trigger retries for GET requests.
#[tokio::test]
async fn get_retries_on_connect_error() {
    // Find a free port by briefly binding then releasing it.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
        // l is dropped here, releasing the port
    };

    // Start the server after a brief pause, giving the first connect attempt
    // time to fail with "connection refused".
    tokio::spawn(async move {
        // Wait longer than the jitter delay (BASE_DELAY_MS = 1ms in tests).
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let listener = TcpListener::bind(format!("127.0.0.1:{port}")).await.unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        drain_request(&mut stream).await;
        send_response(&mut stream, 200, r#"{"connected":true}"#).await;
    });

    let client = RemoteClient::new(&format!("http://127.0.0.1:{port}"), "test-key");
    let result: serde_json::Value = client.get("/api/ping").await.expect("should succeed after retry");
    assert_eq!(result["connected"], serde_json::Value::Bool(true));
}
