//! WebSocket event streaming handler.
//!
//! Endpoint:
//!   - `GET /ws/events?key=<api_key_or_jwt>` — upgrades the connection to WebSocket
//!     and streams [`super::BroadcastEvent`] messages filtered by the client's
//!     [`SubscriptionFilter`].
//!
//! ## Authentication
//! WebSocket connections authenticate via the `key` query parameter (JWT, admin
//! key, or plain API key — same precedence as the REST middleware).
//!
//! ## Subscription
//! After connecting the client must send a subscribe message:
//! ```json
//! { "subscribe": { "event_types": ["WorkspaceCreated"], "workspaces": [] } }
//! ```
//! An empty list for any field means "match all". The client can update the
//! filter at any time by sending another subscribe message.
//!
//! ## Reconnection / replay
//! Clients may pass `?last_event_id=N` to replay events missed while
//! disconnected.  In SQLite mode the in-memory [`super::EventBuffer`] is used;
//! in Postgres mode the database is queried directly.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query as AxumQuery, State};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use utoipa::ToSchema;

use super::{AgentIdentity, ApiError, AppState, AuthSource, BroadcastEvent, EventBuffer, RepoCtx};
use super::require_repo_permission;
use crate::storage::{EventFilter, EventStore as _};

// ── Subscription types ────────────────────────────────────────────────────────

/// Subscription filter sent by the client after connecting.
///
/// An empty list for any field means "match all" for that dimension.
#[derive(Debug, Default, Clone, Deserialize, Serialize, ToSchema)]
pub struct SubscriptionFilter {
    /// Match events touching any of these entity IDs.
    #[serde(default)]
    pub entities: Vec<String>,
    /// Match events touching any of these file paths (glob patterns not yet supported).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Match only these event type names (e.g. `"WorkspaceCreated"`).
    #[serde(default)]
    pub event_types: Vec<String>,
    /// Match only events from these workspace IDs.
    #[serde(default)]
    pub workspaces: Vec<String>,
}

/// Incoming WebSocket message from a client.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ClientMessage {
    Subscribe { subscribe: SubscriptionFilter },
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

/// Query parameters for the WebSocket upgrade request.
#[derive(Debug, Deserialize)]
pub(super) struct WsQueryParams {
    pub(super) key: Option<String>,
    /// The `event_id` of the last event the agent received before disconnecting.
    ///
    /// When present, the server replays all buffered events that occurred after
    /// this ID (filtered by the agent's subscription).  If the buffer has been
    /// exceeded a `{"buffer_exceeded": true}` message is sent first so the
    /// agent knows to perform a full sync.
    pub(super) last_event_id: Option<u64>,
}

/// `GET /ws/events?key=<api_key_or_jwt>` — upgrades the connection to WebSocket and
/// begins streaming events matching the client's subscription filter.
///
/// Authentication is via the `key` query parameter. Accepted formats (checked
/// in this order): JWT access token (contains `'.'`), admin bootstrap key,
/// or a plain API key string looked up in the key store.
///
/// After connecting, the client must send a subscribe message:
///
/// ```json
/// { "subscribe": { "event_types": ["WorkspaceCreated"], "workspaces": [] } }
/// ```
///
/// An empty list for any field means "match all". Events are delivered as JSON
/// matching [`BroadcastEvent`]. The client can send updated subscribe messages
/// at any time to change the filter.
#[utoipa::path(
    get,
    path = "/ws/events",
    params(
        ("key" = String, Query, description = "Authentication token — JWT access token or API key"),
        ("last_event_id" = Option<u64>, Query, description = "Last received event ID for replay on reconnect"),
    ),
    responses(
        (status = 101, description = "WebSocket connection upgraded — events stream as JSON BroadcastEvent messages"),
        (status = 401, description = "Unauthorized — missing or invalid key"),
    ),
    tag = "status"
)]
pub(super) async fn ws_events_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    AxumQuery(params): AxumQuery<WsQueryParams>,
) -> Response {
    let key_str = match params.key {
        Some(k) => k,
        None => {
            return ApiError::unauthorized(
                "missing `key` query parameter; use `?key=<api_key>`",
            )
            .into_response();
        }
    };

    // Authenticate the WebSocket connection using the same precedence as
    // `auth_middleware`:
    //   1. JWT — if the key contains '.' treat it as a JWT token.
    //   2. Admin key — compare against the bootstrap admin key.
    //   3. API key — hash and look up via storage backend.
    //
    // Build a full AgentIdentity so we can call require_repo_permission below.
    let identity: AgentIdentity = if key_str.contains('.') {
        // (1) JWT check — validate with JwtService; no database hit.
        use crate::auth::jwt::JwtError;
        match state.jwt_service.verify(&key_str) {
            Ok(claims) => {
                tracing::debug!(actor = %claims.sub, "WebSocket connection authenticated via JWT");
                let user_id = uuid::Uuid::parse_str(&claims.sub).ok();
                let is_admin = claims.role.as_deref() == Some("admin");
                let name = claims.name.unwrap_or_else(|| claims.sub.clone());
                AgentIdentity {
                    key_id: format!("jwt:{}", claims.sub),
                    name,
                    is_admin,
                    user_id,
                    role_override: claims.role,
                    auth_source: AuthSource::Jwt,
                }
            }
            Err(JwtError::Expired) => {
                return ApiError::unauthorized("JWT token has expired").into_response();
            }
            Err(_) => {
                return ApiError::unauthorized("invalid JWT token").into_response();
            }
        }
    } else if key_str == state.admin_key {
        // (2) Admin key — full server access.
        AgentIdentity {
            key_id: "admin".to_string(),
            name: "admin".to_string(),
            is_admin: true,
            user_id: None,
            role_override: None,
            auth_source: AuthSource::AdminKey,
        }
    } else {
        // (3) Validate via storage backend (handles both SQLite and Postgres).
        match state.storage.auth().validate_key(&key_str).await {
            Ok(api_key) => {
                tracing::debug!(agent = %api_key.name, "WebSocket connection authenticated");
                AgentIdentity {
                    key_id: api_key.id,
                    name: api_key.name,
                    is_admin: false,
                    user_id: api_key.user_id,
                    role_override: api_key.role_override,
                    auth_source: AuthSource::ApiKey,
                }
            }
            Err(crate::storage::StorageError::NotFound(_)) => {
                return ApiError::unauthorized("invalid or revoked API key").into_response();
            }
            Err(e) => {
                return ApiError::internal(format!("auth error: {e}")).into_response();
            }
        }
    };

    // Verify the caller has at least Read access to this repo before upgrading.
    // In SQLite (local) mode this is a no-op; in Postgres mode it checks the
    // collaborator table so cross-tenant WS connections are rejected.
    if let Err(e) = require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await
    {
        return e.into_response();
    }

    let agent_name = identity.name;

    let last_event_id = params.last_event_id;

    // In server mode (Postgres), use LISTEN/NOTIFY-driven delivery.
    // In local mode (SQLite), fall back to the in-memory broadcast channel.
    match ctx.storage {
        crate::storage::StorageBackend::Server(ref pg)
        | crate::storage::StorageBackend::ServerWithS3(ref pg, _)
        | crate::storage::StorageBackend::ServerWithMemFs(ref pg, _) => {
            let pg = Arc::clone(pg);
            let repo_id = ctx.repo_id;
            ws.on_upgrade(move |socket| {
                handle_ws_connection_pg(socket, pg, repo_id, agent_name, last_event_id)
            })
        }
        crate::storage::StorageBackend::Local(_) => {
            let event_rx = state.event_tx.subscribe();
            let event_buffer = Arc::clone(&state.event_buffer);
            ws.on_upgrade(move |socket| {
                handle_ws_connection(socket, event_rx, agent_name, event_buffer, last_event_id)
            })
        }
    }
}

/// Converts a WebSocket [`SubscriptionFilter`] into a storage [`EventFilter`]
/// so filter conditions can be pushed to the database layer.
///
/// Workspace IDs that cannot be parsed as UUIDs are silently dropped — they
/// cannot match any stored row.
fn subscription_to_event_filter(sub: &SubscriptionFilter) -> EventFilter {
    let workspace_ids = sub
        .workspaces
        .iter()
        .filter_map(|s| s.parse::<uuid::Uuid>().ok())
        .collect();
    EventFilter {
        event_types: sub.event_types.clone(),
        workspace_ids,
        entity_ids: sub.entities.clone(),
        paths: sub.paths.clone(),
    }
}

/// Returns `true` if `event` passes all non-empty dimensions of `filter`.
fn filter_matches(filter: &SubscriptionFilter, event: &BroadcastEvent) -> bool {
    // Event-type filter.
    if !filter.event_types.is_empty()
        && !filter.event_types.iter().any(|t| t == &event.event_type)
    {
        return false;
    }

    // Workspace filter.
    if !filter.workspaces.is_empty() {
        match &event.workspace_id {
            Some(ws) if filter.workspaces.contains(ws) => {}
            _ => return false,
        }
    }

    // Entity filter: check if any entity ID appears in event.data.
    if !filter.entities.is_empty() {
        let data_str = event.data.to_string();
        if !filter.entities.iter().any(|eid| data_str.contains(eid.as_str())) {
            return false;
        }
    }

    // Path filter: check if any path appears in event.data.
    if !filter.paths.is_empty() {
        let data_str = event.data.to_string();
        if !filter.paths.iter().any(|p| data_str.contains(p.as_str())) {
            return false;
        }
    }

    true
}

/// Manages a single WebSocket client connection.
///
/// Spawns a receiver task to handle incoming subscription messages while the
/// main task forwards matching events from the broadcast channel.
///
/// If `last_event_id` is `Some`, the server replays buffered events that the
/// agent missed since that ID (filtered by the agent's subscription filter).
/// The replay happens immediately after the first subscribe message arrives.
/// If the replay buffer has been exceeded a `{"buffer_exceeded": true}` JSON
/// message is sent before the replayed events so the agent knows to sync.
async fn handle_ws_connection(
    socket: WebSocket,
    mut event_rx: broadcast::Receiver<BroadcastEvent>,
    agent_name: String,
    event_buffer: Arc<StdMutex<EventBuffer>>,
    last_event_id: Option<u64>,
) {
    let (ws_tx, ws_rx) = socket.split();

    // Wrap the sender in Arc<Mutex> so it can be shared across tasks.
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // The current subscription filter, shared between the receiver task
    // (which updates it) and the event-forwarding loop (which reads it).
    // `None` means the client has not yet sent a subscribe message.
    let filter: Arc<Mutex<Option<SubscriptionFilter>>> = Arc::new(Mutex::new(None));
    let filter_for_recv = Arc::clone(&filter);
    let ws_tx_for_recv = Arc::clone(&ws_tx);

    // Whether the missed-event replay has already been performed for this
    // connection.  Reset to false on fresh connects (last_event_id == None).
    let replay_done = Arc::new(std::sync::atomic::AtomicBool::new(last_event_id.is_none()));
    let replay_done_for_recv = Arc::clone(&replay_done);

    // Spawn a task to handle incoming client messages (subscription updates).
    let recv_task = tokio::spawn(async move {
        let mut ws_rx = ws_rx;
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Subscribe { subscribe }) => {
                            tracing::debug!(
                                agent = %agent_name,
                                event_types = ?subscribe.event_types,
                                "WebSocket subscription updated"
                            );

                            // On the first subscribe message of a reconnection,
                            // replay any buffered events the agent missed.
                            if !replay_done_for_recv.swap(true, Ordering::Relaxed) {
                                if let Some(last_id) = last_event_id {
                                    let (buffer_exceeded, missed) = {
                                        match event_buffer.lock() {
                                            Ok(buf) => buf.events_since(last_id),
                                            Err(_) => (true, vec![]),
                                        }
                                    };

                                    let mut sender = ws_tx_for_recv.lock().await;

                                    if buffer_exceeded {
                                        let flag = serde_json::json!({ "buffer_exceeded": true })
                                            .to_string();
                                        if sender
                                            .send(Message::Text(flag))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                        tracing::info!(
                                            agent = %agent_name,
                                            last_event_id,
                                            "replay buffer exceeded; agent should sync"
                                        );
                                    }

                                    let count = missed.len();
                                    for event in missed {
                                        if filter_matches(&subscribe, &event) {
                                            match serde_json::to_string(&event) {
                                                Ok(json) => {
                                                    if sender
                                                        .send(Message::Text(json))
                                                        .await
                                                        .is_err()
                                                    {
                                                        return;
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        "failed to serialize replayed event: {e}"
                                                    );
                                                }
                                            }
                                        }
                                    }

                                    tracing::debug!(
                                        agent = %agent_name,
                                        replayed = count,
                                        "replayed missed events after reconnect"
                                    );
                                }
                            }

                            *filter_for_recv.lock().await = Some(subscribe);
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "invalid WebSocket message");
                            // Send an error back to the client.
                            let err_msg = serde_json::json!({ "error": format!("{e}") })
                                .to_string();
                            let _ = ws_tx_for_recv
                                .lock()
                                .await
                                .send(Message::Text(err_msg))
                                .await;
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {} // Ping/Pong/Binary — ignore.
            }
        }
    });

    // Forward matching events to the client until the channel closes or the
    // client disconnects.
    loop {
        match event_rx.recv().await {
            Ok(event) => {
                // Check filter (None = not yet subscribed, drop all events).
                let should_send = {
                    let guard = filter.lock().await;
                    guard
                        .as_ref()
                        .is_some_and(|f| filter_matches(f, &event))
                };

                if should_send {
                    let json = match serde_json::to_string(&event) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("failed to serialize broadcast event: {e}");
                            continue;
                        }
                    };
                    let send_result = ws_tx.lock().await.send(Message::Text(json)).await;
                    if send_result.is_err() {
                        break; // Client disconnected.
                    }
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "WebSocket client lagged behind event stream");
            }
        }
    }

    recv_task.abort();
}

/// Converts a storage [`crate::event_log::Event`] into the [`BroadcastEvent`]
/// wire format delivered to WebSocket clients.
fn event_to_broadcast(event: crate::event_log::Event) -> BroadcastEvent {
    let workspace_id = event.kind.workspace_id().map(|id| id.to_string());
    let event_type = event.kind.event_type().to_string();
    let data = serde_json::to_value(&event.kind).unwrap_or(serde_json::Value::Null);
    BroadcastEvent {
        event_type,
        event_id: event.id,
        workspace_id,
        timestamp: event.timestamp.to_rfc3339(),
        data,
    }
}

/// Creates a fresh [`sqlx::postgres::PgListener`] subscribed to `vai_events`.
///
/// Returns `None` and logs the error if either the connection or the
/// `LISTEN` command fails.
async fn create_pg_listener(
    pg: &crate::storage::postgres::PostgresStorage,
) -> Option<sqlx::postgres::PgListener> {
    match pg.create_listener().await {
        Ok(mut l) => match l.listen("vai_events").await {
            Ok(()) => Some(l),
            Err(e) => {
                tracing::error!("failed to LISTEN on vai_events: {e}");
                None
            }
        },
        Err(e) => {
            tracing::error!("failed to create PgListener: {e}");
            None
        }
    }
}

/// Manages a WebSocket connection backed by Postgres LISTEN/NOTIFY.
///
/// This is the server-mode counterpart to [`handle_ws_connection`]. Instead of
/// reading from an in-memory broadcast channel it:
///
/// 1. Creates a [`sqlx::postgres::PgListener`] on the `vai_events` channel.
/// 2. When the client sends a `subscribe` message and a `last_event_id` was
///    provided on connect, queries the database for missed events and delivers
///    them before switching to live delivery.
/// 3. On each `NOTIFY vai_events, '<repo_id>:<event_id>'`, queries all events
///    since the last delivered ID for the subscribed repo, applies the client's
///    subscription filter, and sends matching events.
///
/// ## Reliability
/// - **Keepalive**: if no NOTIFY arrives for 60 s the listener is recreated so
///   the pool's idle-timeout cannot silently close the underlying connection.
/// - **Reconnection**: on `recv()` errors the listener is recreated with
///   exponential backoff (1 s → 2 s → … capped at 30 s). The WebSocket is
///   closed after [`MAX_RECONNECT`] consecutive failures.
/// - **Query errors** do NOT advance `last_delivered_id` so no events are lost.
/// - **recv_task monitoring**: the main loop exits when the client recv task
///   finishes, ensuring clean shutdown on client disconnect.
/// - **WebSocket ping/pong**: a ping frame is sent every 30 s; if no pong
///   arrives within 10 s the connection is closed (catches silent TCP drops).
async fn handle_ws_connection_pg(
    socket: WebSocket,
    pg: Arc<crate::storage::postgres::PostgresStorage>,
    repo_id: uuid::Uuid,
    agent_name: String,
    last_event_id: Option<u64>,
) {
    /// Seconds between keepalive listener recreations when no NOTIFY arrives.
    const KEEPALIVE_SECS: u64 = 60;
    /// Seconds between outgoing WebSocket ping frames.
    const PING_SECS: u64 = 30;
    /// Seconds to wait for a pong before closing the connection.
    const PONG_TIMEOUT_SECS: u64 = 10;
    /// Maximum consecutive PgListener failures before giving up.
    const MAX_RECONNECT: u32 = 5;

    let (ws_tx, ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // Shared subscription filter — `None` until the client sends Subscribe.
    let filter: Arc<Mutex<Option<SubscriptionFilter>>> = Arc::new(Mutex::new(None));
    let filter_for_recv = Arc::clone(&filter);
    let ws_tx_for_recv = Arc::clone(&ws_tx);

    // Tracks the highest event ID we have delivered to this client.
    let last_delivered_id = Arc::new(AtomicU64::new(last_event_id.unwrap_or(0)));
    let last_delivered_for_recv = Arc::clone(&last_delivered_id);

    // Whether the missed-event replay has already been triggered.
    let replay_done = Arc::new(std::sync::atomic::AtomicBool::new(last_event_id.is_none()));
    let replay_done_for_recv = Arc::clone(&replay_done);

    let pg_for_recv = Arc::clone(&pg);

    // Channel to relay WebSocket Pong frames from recv_task to the main loop.
    let (pong_tx, mut pong_rx) = tokio::sync::mpsc::channel::<()>(4);

    // Spawn a task that reads incoming client messages (subscription updates).
    let mut recv_task = tokio::spawn(async move {
        let mut ws_rx = ws_rx;
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Pong(_)) => {
                    // Signal the main loop that a pong was received.
                    let _ = pong_tx.try_send(());
                }
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Subscribe { subscribe }) => {
                            tracing::debug!(
                                agent = %agent_name,
                                event_types = ?subscribe.event_types,
                                "WebSocket subscription updated (Postgres mode)"
                            );

                            // On the first subscribe message of a reconnection,
                            // replay any events the client missed — applying the
                            // subscription filter in the database query.
                            if !replay_done_for_recv.swap(true, Ordering::Relaxed) {
                                if let Some(last_id) = last_event_id {
                                    let ev_filter = subscription_to_event_filter(&subscribe);
                                    match pg_for_recv
                                        .query_since_id_filtered(
                                            &repo_id,
                                            last_id as i64,
                                            &ev_filter,
                                        )
                                        .await
                                    {
                                        Ok(events) => {
                                            let mut sender = ws_tx_for_recv.lock().await;
                                            let mut max_id = last_id;
                                            for event in events {
                                                let bc = event_to_broadcast(event);
                                                if bc.event_id > max_id {
                                                    max_id = bc.event_id;
                                                }
                                                match serde_json::to_string(&bc) {
                                                    Ok(json) => {
                                                        if sender
                                                            .send(Message::Text(json))
                                                            .await
                                                            .is_err()
                                                        {
                                                            return;
                                                        }
                                                    }
                                                    Err(e) => tracing::error!(
                                                        "replay serialize error: {e}"
                                                    ),
                                                }
                                            }
                                            // Advance the cursor past replayed events.
                                            last_delivered_for_recv
                                                .fetch_max(max_id, Ordering::Relaxed);
                                        }
                                        Err(e) => {
                                            tracing::error!("replay query failed: {e}");
                                        }
                                    }
                                }
                            }

                            *filter_for_recv.lock().await = Some(subscribe);
                        }
                        Err(e) => {
                            let err_msg =
                                serde_json::json!({ "error": format!("{e}") }).to_string();
                            let _ = ws_tx_for_recv
                                .lock()
                                .await
                                .send(Message::Text(err_msg))
                                .await;
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    // Create the initial PgListener.
    let mut listener = match create_pg_listener(&pg).await {
        Some(l) => l,
        None => {
            recv_task.abort();
            return;
        }
    };

    let mut ping_interval =
        tokio::time::interval(std::time::Duration::from_secs(PING_SECS));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Deadline by which a pong must arrive; `None` when no ping is outstanding.
    let mut pong_deadline: Option<tokio::time::Instant> = None;
    let mut reconnect_count = 0u32;

    'main: loop {
        tokio::select! {
            // Monitor recv_task — exit cleanly when the client disconnects.
            _ = &mut recv_task => { break 'main; }

            // Postgres LISTEN/NOTIFY with a keepalive timeout.
            result = tokio::time::timeout(
                std::time::Duration::from_secs(KEEPALIVE_SECS),
                listener.recv(),
            ) => {
                match result {
                    // Timeout: no NOTIFY for KEEPALIVE_SECS.  Recreate the
                    // listener to prevent the pool's idle-timeout from closing
                    // the underlying connection silently.
                    Err(_timeout) => {
                        tracing::debug!("PgListener keepalive: recreating listener");
                        match create_pg_listener(&pg).await {
                            Some(new_listener) => {
                                listener = new_listener;
                                reconnect_count = 0;
                            }
                            None => {
                                reconnect_count += 1;
                                if reconnect_count >= MAX_RECONNECT {
                                    tracing::error!(
                                        "PgListener keepalive failed {MAX_RECONNECT} times, closing WebSocket"
                                    );
                                    break 'main;
                                }
                            }
                        }
                    }

                    // NOTIFY received — forward matching events to the client.
                    Ok(Ok(notification)) => {
                        reconnect_count = 0;

                        // Payload format: "<repo_id>:<event_id>"
                        let payload = notification.payload();
                        let Some((repo_str, event_id_str)) = payload.split_once(':') else {
                            continue;
                        };
                        let Ok(notif_repo) = repo_str.parse::<uuid::Uuid>() else {
                            continue;
                        };
                        let Ok(notif_event_id) = event_id_str.parse::<i64>() else {
                            continue;
                        };
                        // Only handle NOTIFYs for this client's repo.
                        if notif_repo != repo_id {
                            continue;
                        }

                        // Gate delivery: client must have subscribed first.
                        let current_filter = {
                            let guard = filter.lock().await;
                            guard.clone()
                        };
                        let Some(ref sub) = current_filter else {
                            continue;
                        };

                        let ev_filter = subscription_to_event_filter(sub);
                        let since = last_delivered_id.load(Ordering::Relaxed) as i64;
                        let events = match pg
                            .query_since_id_filtered(&repo_id, since, &ev_filter)
                            .await
                        {
                            Ok(e) => e,
                            Err(e) => {
                                // Do NOT advance last_delivered_id on error — events
                                // must not be permanently skipped on a transient failure.
                                tracing::error!("query_since_id_filtered failed: {e}");
                                continue;
                            }
                        };

                        for event in events {
                            let bc = event_to_broadcast(event);
                            let json = match serde_json::to_string(&bc) {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!("serialize event failed: {e}");
                                    continue;
                                }
                            };
                            if ws_tx.lock().await.send(Message::Text(json)).await.is_err() {
                                break 'main;
                            }
                        }

                        // Advance cursor to the notified event ID so subsequent
                        // NOTIFYs don't re-scan already-considered events.
                        last_delivered_id
                            .fetch_max(notif_event_id as u64, Ordering::Relaxed);
                    }

                    // recv() error — reconnect with exponential backoff.
                    Ok(Err(e)) => {
                        tracing::error!("PgListener recv error: {e}");
                        reconnect_count += 1;
                        if reconnect_count >= MAX_RECONNECT {
                            tracing::error!(
                                "PgListener failed {MAX_RECONNECT} consecutive times, closing WebSocket"
                            );
                            break 'main;
                        }
                        let backoff = std::time::Duration::from_secs(
                            (1u64 << (reconnect_count - 1)).min(30),
                        );
                        tracing::info!(
                            attempt = reconnect_count,
                            delay_secs = backoff.as_secs(),
                            "reconnecting PgListener"
                        );
                        tokio::time::sleep(backoff).await;
                        if let Some(new_listener) = create_pg_listener(&pg).await {
                            listener = new_listener;
                        }
                        // If reconnect fails, the next iteration will try again
                        // until MAX_RECONNECT is reached.
                    }
                }
            }

            // Send a WebSocket ping frame on each tick.
            _ = ping_interval.tick() => {
                // Close the connection if the previous ping timed out.
                if let Some(deadline) = pong_deadline {
                    if tokio::time::Instant::now() >= deadline {
                        tracing::warn!("WebSocket pong timeout, closing connection");
                        break 'main;
                    }
                }
                // Drain any already-queued pongs before issuing a new ping so
                // we don't confuse stale pongs with the one we're requesting.
                while pong_rx.try_recv().is_ok() {}
                if ws_tx
                    .lock()
                    .await
                    .send(Message::Ping(vec![]))
                    .await
                    .is_err()
                {
                    break 'main;
                }
                pong_deadline = Some(
                    tokio::time::Instant::now()
                        + std::time::Duration::from_secs(PONG_TIMEOUT_SECS),
                );
            }

            // Pong received — cancel the outstanding deadline.
            Some(()) = pong_rx.recv() => {
                pong_deadline = None;
            }
        }
    }

    recv_task.abort();
}
