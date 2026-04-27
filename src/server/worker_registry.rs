//! Worker registry — capacity checks, spawn orchestration, and dead-worker
//! reconciliation (PRD 28).
//!
//! The primary entry point is [`spawn_if_capacity`], called by the
//! issue-creation handler after a new issue is persisted.
//!
//! [`run_reconciliation_loop`] is a background task that periodically finds
//! stale workers (no heartbeat for `VAI_WORKER_STALE_SECS`, default 900 s)
//! and marks them dead, discarding their claimed workspaces so the linked
//! issues requeue automatically.

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::storage::{NewWorker, WorkerDoneReason, WorkerStore};

use super::compute::{ComputeProvider, MachineId, ProviderError, WorkerSpec};

/// Hard-coded per-repo concurrency cap used until org-level plan billing lands
/// (PRD 28 Phase 4).  Mirrors the `free` tier in the `plans` table.
const DEFAULT_MAX_CONCURRENT: u64 = 3;

/// Abstracts over per-repo secret retrieval for [`spawn_if_capacity`].
///
/// Implemented for [`sqlx::PgPool`] (when the `postgres` feature is enabled)
/// via [`super::secrets`], and by [`NoopSecretsStore`] for test / non-postgres
/// builds.
#[async_trait]
pub(crate) trait SecretsStore: Send + Sync {
    /// Retrieve all secrets for `repo_id` as a decrypted key→value map.
    ///
    /// Used to bulk-inject repo secrets into worker environment variables so
    /// tools like Vite that read env vars at startup can find `DATABASE_URL`,
    /// `BETTER_AUTH_SECRET`, etc. without a `.env` file on disk.
    async fn list_and_get_all(
        &self,
        repo_id: &Uuid,
    ) -> Result<std::collections::HashMap<String, String>, String>;
}

/// No-op secrets store — always returns empty results.
///
/// Used in local / test builds where no Postgres vault is available.
pub(crate) struct NoopSecretsStore;

#[async_trait]
impl SecretsStore for NoopSecretsStore {
    async fn list_and_get_all(
        &self,
        _repo_id: &Uuid,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        Ok(std::collections::HashMap::new())
    }
}

/// Postgres vault implementation — delegates to [`super::secrets::get_all_secrets`].
#[cfg(feature = "postgres")]
#[async_trait]
impl SecretsStore for sqlx::PgPool {
    async fn list_and_get_all(
        &self,
        repo_id: &Uuid,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        super::secrets::get_all_secrets(self, repo_id)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Configuration passed to [`spawn_if_capacity`].
pub struct SpawnConfig<'a> {
    /// OCI image reference for the canonical worker (e.g. `ghcr.io/jjordy/vai-worker:v1.2.3`).
    pub worker_image: &'a str,
    /// Public URL of the vai server, injected as `VAI_SERVER_URL` in the worker.
    pub server_url: &'a str,
    /// Human-readable repository name, injected as `VAI_REPO` in the worker.
    pub repo_name: &'a str,
    /// Short-lived API key the worker will use to authenticate against this server.
    pub vai_api_key: &'a str,
}

/// Errors from spawn operations.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),
    #[error("compute provider error: {0}")]
    Provider(String),
    /// `ANTHROPIC_API_KEY` was absent from both the per-repo vault and the
    /// server-wide fallback.  The worker was not spawned.
    #[error("required repo secret is not configured: {0}")]
    MissingRepoSecret(&'static str),
}

/// Attempt to spawn a cloud worker for `repo_id`.
///
/// Returns `Some(worker_id)` if a worker was spawned, `None` if the repo has
/// cloud agents disabled or is already at the concurrency cap.
///
/// The Anthropic API key is resolved in priority order:
/// 1. Per-repo vault lookup via `secrets` (key `ANTHROPIC_API_KEY`).
/// 2. Server-wide `fallback_key` (from `ANTHROPIC_API_KEY` env var at startup).
/// 3. [`RegistryError::MissingRepoSecret`] if both are absent — no worker row
///    is created and no machine is booted.
///
/// The function is **fire-and-continue**: it inserts the `agent_workers` row
/// before the machine actually boots, so callers can return the new issue
/// immediately without blocking on provider latency.
pub async fn spawn_if_capacity(
    repo_id: &Uuid,
    compute: &dyn ComputeProvider,
    workers: Arc<dyn WorkerStore>,
    secrets: Arc<dyn SecretsStore>,
    fallback_key: &str,
    config: &SpawnConfig<'_>,
) -> Result<Option<Uuid>, RegistryError> {
    // Check cloud_agent_enabled on the repo.
    if !workers.is_cloud_agent_enabled(repo_id).await? {
        return Ok(None);
    }

    // Check concurrency cap.
    let cap = configured_max_concurrent();
    let running = workers.count_running_workers(repo_id).await?;
    if running >= cap {
        tracing::debug!(
            repo_id = %repo_id,
            running,
            cap,
            "cloud agent concurrency cap reached — skipping spawn"
        );
        return Ok(None);
    }

    // Fetch all per-repo secrets from the vault.  Every key is injected into the
    // worker env so tools like Vite (DATABASE_URL, BETTER_AUTH_SECRET, …) can
    // start without a .env file on disk.
    let mut vault_secrets = secrets
        .list_and_get_all(repo_id)
        .await
        .map_err(RegistryError::Provider)?;

    // Resolve ANTHROPIC_API_KEY: vault value takes precedence; fall back to the
    // server-wide key; error if neither is present.
    if !vault_secrets.contains_key("ANTHROPIC_API_KEY") {
        if !fallback_key.is_empty() {
            vault_secrets.insert("ANTHROPIC_API_KEY".into(), fallback_key.to_string());
        } else {
            return Err(RegistryError::MissingRepoSecret("ANTHROPIC_API_KEY"));
        }
    }

    // Insert the worker row in 'spawning' state before calling the provider,
    // so a crash after spawn but before insert doesn't orphan a live machine.
    // We need the worker UUID before building the env so it can be injected
    // as VAI_WORKER_ID for the worker's heartbeat / log / done calls.
    let worker = workers
        .create_worker(NewWorker {
            repo_id: *repo_id,
            provider: "fly".to_string(),
            machine_id: None,
        })
        .await?;

    tracing::info!(
        event = "worker_registry.pre_spawn",
        worker_id = %worker.id,
        repo_id = %repo_id,
        "worker row created, about to call compute.spawn"
    );

    // Mint a unique idempotency key for this spawn attempt.
    let idempotency_key = Uuid::new_v4().to_string();

    // Build environment for the worker: start from all decrypted repo secrets,
    // then layer the vai-specific vars on top (they take precedence).
    let mut env = vault_secrets;
    env.insert("VAI_SERVER_URL".into(), config.server_url.to_string());
    env.insert("VAI_REPO".into(), config.repo_name.to_string());
    env.insert("VAI_API_KEY".into(), config.vai_api_key.to_string());
    env.insert("VAI_REPO_ID".into(), repo_id.to_string());
    env.insert("VAI_WORKER_ID".into(), worker.id.to_string());

    let spec = WorkerSpec {
        image: config.worker_image.to_string(),
        env,
        resources: super::compute::ResourceClass::Large,
        labels: {
            let mut l = std::collections::HashMap::new();
            l.insert("vai_repo_id".into(), repo_id.to_string());
            l
        },
        idempotency_key,
    };

    // Spawn the machine.  On failure we leave the row in 'spawning'; the
    // dead-worker reconciliation cron will clean it up.
    let machine_id: MachineId = match compute.spawn(spec).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                worker_id = %worker.id,
                repo_id = %repo_id,
                error = %e,
                "compute provider spawn failed"
            );
            return Err(RegistryError::Provider(e.to_string()));
        }
    };

    // Back-fill machine_id now that the provider assigned one.
    // If this update fails the reconciliation cron will still catch the row.
    let _ = workers.set_machine_id(&worker.id, &machine_id.0).await;

    tracing::info!(
        worker_id = %worker.id,
        machine_id = %machine_id,
        repo_id = %repo_id,
        "cloud worker spawned"
    );

    Ok(Some(worker.id))
}

/// Default stale threshold in seconds (5 minutes = 10 missed heartbeats at
/// the default 30 s interval).
const DEFAULT_STALE_SECS: u32 = 900;

/// Reconcile one pass: find stale workers, discard their workspaces, reopen
/// their issues, mark them dead, and destroy their Fly machines.
///
/// Returns the number of workers marked dead.
async fn reconcile_once(
    storage: &crate::storage::StorageBackend,
    stale_secs: u32,
    compute: &Arc<dyn ComputeProvider>,
) -> u32 {
    let workers = storage.workers();
    let stale = match workers.list_stale_workers(stale_secs).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "dead-worker reconciliation: list_stale_workers failed");
            return 0;
        }
    };

    let mut marked = 0u32;
    for worker in stale {
        // If the worker holds a workspace, discard it so the linked issue
        // transitions back to Open.
        if let Some(ws_id) = worker.workspace_id {
            let workspaces = storage.workspaces();
            let ws = workspaces.get_workspace(&worker.repo_id, &ws_id).await;
            match ws {
                Ok(meta) => {
                    if let Err(e) = workspaces
                        .discard_workspace(&worker.repo_id, &ws_id)
                        .await
                    {
                        tracing::warn!(
                            worker_id = %worker.id,
                            workspace_id = %ws_id,
                            error = %e,
                            "dead-worker reconciliation: discard_workspace failed"
                        );
                    } else if let Some(issue_id) = meta.issue_id {
                        let _ = storage
                            .issues()
                            .update_issue(
                                &worker.repo_id,
                                &issue_id,
                                crate::storage::IssueUpdate {
                                    status: Some(crate::issue::IssueStatus::Open),
                                    ..Default::default()
                                },
                            )
                            .await;
                        tracing::info!(
                            worker_id = %worker.id,
                            workspace_id = %ws_id,
                            issue_id = %issue_id,
                            "dead-worker reconciliation: workspace discarded, issue reopened"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        worker_id = %worker.id,
                        workspace_id = %ws_id,
                        error = %e,
                        "dead-worker reconciliation: get_workspace failed"
                    );
                }
            }
        }

        match workers.mark_done(&worker.id, WorkerDoneReason::Terminated).await {
            Ok(()) => {
                tracing::info!(
                    worker_id = %worker.id,
                    machine_id = ?worker.machine_id,
                    state = %worker.state,
                    "dead-worker reconciliation: worker marked dead"
                );
                marked += 1;

                // Destroy the Fly machine so it stops burning compute quota.
                if let Some(mid) = &worker.machine_id {
                    let mid = MachineId(mid.clone());
                    match compute.destroy(&mid).await {
                        Ok(()) => {
                            tracing::info!(
                                worker_id = %worker.id,
                                machine_id = %mid,
                                "dead-worker reconciliation: machine destroyed"
                            );
                        }
                        Err(ProviderError::NotFound(_)) => {
                            // Machine already gone — nothing to do.
                        }
                        Err(e) => {
                            tracing::warn!(
                                worker_id = %worker.id,
                                machine_id = %mid,
                                error = %e,
                                "dead-worker reconciliation: destroy machine failed (non-fatal)"
                            );
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    worker_id = %worker.id,
                    error = %e,
                    "dead-worker reconciliation: mark_done failed"
                );
            }
        }
    }

    marked += sweep_orphaned_workspaces(storage, stale_secs).await;
    marked
}

/// Sweep workspaces that are in `Created`/`Active` state, linked to an issue,
/// but have no live worker claiming them (i.e. the worker row never had its
/// `workspace_id` set, or the worker was deleted without going through the
/// normal stale-worker path).
///
/// Returns the number of workspaces discarded.
async fn sweep_orphaned_workspaces(
    storage: &crate::storage::StorageBackend,
    stale_secs: u32,
) -> u32 {
    let orphans = match storage.workers().list_orphaned_issue_workspaces(stale_secs).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "orphan sweeper: list_orphaned_issue_workspaces failed");
            return 0;
        }
    };

    let mut swept = 0u32;
    for (ws_id, repo_id, issue_id) in orphans {
        if let Err(e) = storage.workspaces().discard_workspace(&repo_id, &ws_id).await {
            tracing::warn!(
                workspace_id = %ws_id,
                repo_id = %repo_id,
                issue_id = %issue_id,
                error = %e,
                "orphan sweeper: discard_workspace failed"
            );
            continue;
        }

        let _ = storage
            .issues()
            .update_issue(
                &repo_id,
                &issue_id,
                crate::storage::IssueUpdate {
                    status: Some(crate::issue::IssueStatus::Open),
                    ..Default::default()
                },
            )
            .await;

        tracing::info!(
            workspace_id = %ws_id,
            repo_id = %repo_id,
            issue_id = %issue_id,
            "orphan sweeper: workspace discarded, issue reopened"
        );
        swept += 1;
    }
    swept
}

/// Read the per-repo concurrency cap from `VAI_WORKER_MAX_CONCURRENT`, defaulting
/// to [`DEFAULT_MAX_CONCURRENT`].
fn configured_max_concurrent() -> u64 {
    std::env::var("VAI_WORKER_MAX_CONCURRENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_CONCURRENT)
}

// ── Spawn reconciliation loop ─────────────────────────────────────────────────

/// Context required by the spawn reconciliation loop.
pub struct SpawnReconcilerCtx {
    /// Storage backend — used for both worker and issue queries.
    pub storage: crate::storage::StorageBackend,
    /// Compute provider for spawning machines.
    pub compute: Arc<dyn ComputeProvider>,
    /// JWT service used to mint short-lived worker access tokens.
    pub jwt_service: Arc<crate::auth::jwt::JwtService>,
    /// Public URL of this server, injected as `VAI_SERVER_URL` in the worker.
    pub server_url: String,
    /// Anthropic API key fallback used when the per-repo vault has no entry.
    pub anthropic_key: String,
}

/// One reconciliation pass: for every cloud-enabled repo, top up running
/// workers to `min(open_issues, max_concurrent)`.
async fn spawn_reconcile_once(ctx: &SpawnReconcilerCtx) {
    let max_concurrent = configured_max_concurrent();
    let workers = ctx.storage.workers();

    let repos = match workers.list_cloud_enabled_repos().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "spawn reconciler: list_cloud_enabled_repos failed");
            return;
        }
    };

    // Build the secrets store once — same for all repos in this tick.
    #[cfg(feature = "postgres")]
    let secrets: Arc<dyn SecretsStore> = {
        match &ctx.storage {
            crate::storage::StorageBackend::Server(pg)
            | crate::storage::StorageBackend::ServerWithS3(pg, _)
            | crate::storage::StorageBackend::ServerWithMemFs(pg, _) => {
                Arc::new(pg.pool().clone())
            }
            _ => Arc::new(NoopSecretsStore),
        }
    };
    #[cfg(not(feature = "postgres"))]
    let secrets: Arc<dyn SecretsStore> = Arc::new(NoopSecretsStore);

    let worker_image = format!("ghcr.io/jjordy/vai-worker:{}", env!("CARGO_PKG_VERSION"));

    for (repo_id, repo_name) in repos {
        let running = match workers.count_running_workers(&repo_id).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    repo_id = %repo_id, error = %e,
                    "spawn reconciler: count_running_workers failed"
                );
                continue;
            }
        };

        if running >= max_concurrent {
            continue;
        }

        // Count open issues to bound how many workers are actually useful.
        let open_count = match ctx
            .storage
            .issues()
            .list_issues(
                &repo_id,
                &crate::issue::IssueFilter {
                    status: Some(vec![crate::issue::IssueStatus::Open]),
                    ..Default::default()
                },
                &crate::storage::ListQuery { page: 1, per_page: 1, sort: vec![] },
            )
            .await
        {
            Ok(r) => r.total,
            Err(e) => {
                tracing::warn!(
                    repo_id = %repo_id, error = %e,
                    "spawn reconciler: list open issues failed"
                );
                continue;
            }
        };

        let needed = open_count.min(max_concurrent).saturating_sub(running);
        if needed == 0 {
            continue;
        }

        tracing::debug!(
            repo_id = %repo_id,
            repo_name = %repo_name,
            running,
            open_count,
            needed,
            "spawn reconciler: topping up workers"
        );

        for _ in 0..needed {
            let worker_token = ctx
                .jwt_service
                .sign(
                    "cloud-worker".to_string(),
                    None,
                    Some(repo_id.to_string()),
                    Some("worker".to_string()),
                )
                .unwrap_or_default();

            let config = SpawnConfig {
                worker_image: &worker_image,
                server_url: &ctx.server_url,
                repo_name: &repo_name,
                vai_api_key: &worker_token,
            };

            match spawn_if_capacity(
                &repo_id,
                ctx.compute.as_ref(),
                ctx.storage.workers(),
                Arc::clone(&secrets),
                &ctx.anthropic_key,
                &config,
            )
            .await
            {
                Ok(Some(worker_id)) => {
                    tracing::info!(
                        worker_id = %worker_id,
                        repo_id = %repo_id,
                        repo_name = %repo_name,
                        "spawn reconciler: worker spawned"
                    );
                }
                Ok(None) => {
                    // Concurrency cap reached or cloud disabled — stop for this repo.
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        repo_id = %repo_id,
                        error = %e,
                        "spawn reconciler: spawn failed (non-fatal)"
                    );
                    break;
                }
            }
        }
    }
}

/// Spawn a background task that periodically tops up cloud workers.
///
/// Interval defaults to 30 s (`VAI_WORKER_RECONCILE_SECS`).  On each tick the
/// reconciler iterates every repo with `cloud_agent_enabled = true` and calls
/// [`spawn_if_capacity`] until `running_workers == min(open_issues, cap)`.
///
/// This ensures that:
/// - Enabling cloud mode on a repo with an existing backlog spawns workers
///   within one tick without requiring a new issue to be filed.
/// - Filing several issues quickly results in up to `cap` concurrent workers,
///   not just one.
///
/// The per-issue-creation webhook in `src/server/issue.rs` is kept as a
/// fast-path: it triggers immediately on issue creation so the first worker
/// starts within seconds rather than waiting for the next tick.
pub fn run_spawn_reconciliation_loop(ctx: SpawnReconcilerCtx) {
    let interval_secs: u64 = std::env::var("VAI_WORKER_RECONCILE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    tokio::spawn(async move {
        let mut tick =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        tracing::info!(
            interval_secs,
            max_concurrent = configured_max_concurrent(),
            "spawn reconciliation loop started"
        );

        loop {
            tick.tick().await;
            spawn_reconcile_once(&ctx).await;
        }
    });
}

/// Spawn a background task that periodically reconciles dead workers.
///
/// Interval defaults to 60 s (`VAI_WORKER_RECONCILE_INTERVAL_SECS`) and stale
/// threshold defaults to 900 s (`VAI_WORKER_STALE_SECS`).
///
/// When a stale worker is reaped, its backing Fly machine is destroyed via
/// `compute` so it stops burning quota.
pub fn run_reconciliation_loop(
    storage: crate::storage::StorageBackend,
    compute: Arc<dyn ComputeProvider>,
) {
    let interval_secs: u64 = std::env::var("VAI_WORKER_RECONCILE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);

    let stale_secs: u32 = std::env::var("VAI_WORKER_STALE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_STALE_SECS);

    tokio::spawn(async move {
        let mut tick =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        tracing::info!(
            interval_secs,
            stale_secs,
            "dead-worker reconciliation loop started"
        );

        loop {
            tick.tick().await;
            let n = reconcile_once(&storage, stale_secs, &compute).await;
            if n > 0 {
                tracing::info!(workers_reaped = n, "dead-worker reconciliation completed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{AgentWorker, LogStream, NewWorker, StorageError, WorkerDoneReason, WorkerLog, WorkerStore};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Mutex;

    // ── Mock secrets store ────────────────────────────────────────────────────

    struct MockSecretsStore {
        secrets: std::collections::HashMap<String, String>,
    }

    impl MockSecretsStore {
        fn with_key(key: impl Into<String>) -> Arc<dyn SecretsStore> {
            let mut m = std::collections::HashMap::new();
            m.insert("ANTHROPIC_API_KEY".into(), key.into());
            Arc::new(Self { secrets: m })
        }

        fn with_secrets(pairs: &[(&str, &str)]) -> Arc<dyn SecretsStore> {
            let secrets = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            Arc::new(Self { secrets })
        }

        fn empty() -> Arc<dyn SecretsStore> {
            Arc::new(Self { secrets: std::collections::HashMap::new() })
        }
    }

    #[async_trait]
    impl SecretsStore for MockSecretsStore {
        async fn list_and_get_all(
            &self,
            _repo_id: &Uuid,
        ) -> Result<std::collections::HashMap<String, String>, String> {
            Ok(self.secrets.clone())
        }
    }

    // ── Mock worker store ─────────────────────────────────────────────────────

    struct MockWorkerStore {
        cloud_enabled: bool,
        running_count: u64,
        spawned: Mutex<Vec<AgentWorker>>,
    }

    impl MockWorkerStore {
        fn new(cloud_enabled: bool, running_count: u64) -> Arc<Self> {
            Arc::new(Self {
                cloud_enabled,
                running_count,
                spawned: Mutex::new(vec![]),
            })
        }
    }

    #[async_trait::async_trait]
    impl WorkerStore for MockWorkerStore {
        async fn create_worker(&self, new: NewWorker) -> Result<AgentWorker, StorageError> {
            let w = AgentWorker {
                id: Uuid::new_v4(),
                repo_id: new.repo_id,
                provider: new.provider,
                machine_id: new.machine_id,
                state: "spawning".to_string(),
                workspace_id: None,
                last_heartbeat_at: None,
                started_at: chrono::Utc::now(),
                ended_at: None,
            };
            self.spawned.lock().await.push(w.clone());
            Ok(w)
        }

        async fn get_worker(&self, _id: &Uuid) -> Result<AgentWorker, StorageError> {
            Err(StorageError::NotFound("worker".to_string()))
        }

        async fn count_running_workers(&self, _repo_id: &Uuid) -> Result<u64, StorageError> {
            Ok(self.running_count)
        }

        async fn is_cloud_agent_enabled(&self, _repo_id: &Uuid) -> Result<bool, StorageError> {
            Ok(self.cloud_enabled)
        }

        async fn list_logs(
            &self,
            _worker_id: &Uuid,
            _since_id: Option<i64>,
        ) -> Result<Vec<WorkerLog>, StorageError> {
            Ok(vec![])
        }

        async fn update_heartbeat(&self, _worker_id: &Uuid) -> Result<(), StorageError> {
            Ok(())
        }

        async fn append_logs(
            &self,
            _worker_id: &Uuid,
            _stream: LogStream,
            _chunks: &[String],
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn set_machine_id(
            &self,
            _worker_id: &Uuid,
            _machine_id: &str,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn mark_done(
            &self,
            _worker_id: &Uuid,
            _reason: WorkerDoneReason,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn get_worker_by_workspace(
            &self,
            _workspace_id: &Uuid,
        ) -> Result<Option<AgentWorker>, StorageError> {
            Ok(None)
        }

        async fn list_stale_workers(
            &self,
            _stale_secs: u32,
        ) -> Result<Vec<AgentWorker>, StorageError> {
            Ok(vec![])
        }

        async fn set_workspace_id(
            &self,
            _worker_id: &Uuid,
            _workspace_id: &Uuid,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn list_orphaned_issue_workspaces(
            &self,
            _stale_secs: u32,
        ) -> Result<Vec<(Uuid, Uuid, Uuid)>, StorageError> {
            Ok(vec![])
        }

        async fn set_cloud_agent_enabled(
            &self,
            _repo_id: &Uuid,
            _enabled: bool,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn list_cloud_enabled_repos(&self) -> Result<Vec<(Uuid, String)>, StorageError> {
            Ok(vec![])
        }

        async fn list_workers_by_repo(
            &self,
            _repo_id: &Uuid,
            _state_filter: Option<&str>,
            _query: &crate::storage::pagination::ListQuery,
        ) -> Result<crate::storage::ListResult<AgentWorker>, StorageError> {
            Ok(crate::storage::ListResult { items: vec![], total: 0 })
        }
    }

    // ── Mock compute provider ─────────────────────────────────────────────────

    struct MockCompute {
        spawned: AtomicBool,
    }

    impl MockCompute {
        fn new() -> Arc<Self> {
            Arc::new(Self { spawned: AtomicBool::new(false) })
        }

        fn was_spawned(&self) -> bool {
            self.spawned.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ComputeProvider for MockCompute {
        async fn spawn(
            &self,
            _spec: WorkerSpec,
        ) -> Result<MachineId, super::super::compute::ProviderError> {
            self.spawned.store(true, Ordering::SeqCst);
            Ok(MachineId(Uuid::new_v4().to_string()))
        }

        async fn destroy(
            &self,
            _id: &MachineId,
        ) -> Result<(), super::super::compute::ProviderError> {
            Ok(())
        }

        async fn describe(
            &self,
            _id: &MachineId,
        ) -> Result<super::super::compute::WorkerStatus, super::super::compute::ProviderError> {
            Err(super::super::compute::ProviderError::NotFound(_id.clone()))
        }

        async fn list(
            &self,
            _labels: &super::super::compute::WorkerLabels,
        ) -> Result<Vec<super::super::compute::WorkerSummary>, super::super::compute::ProviderError>
        {
            Ok(vec![])
        }
    }

    fn test_config() -> SpawnConfig<'static> {
        SpawnConfig {
            worker_image: "ghcr.io/jjordy/vai-worker:test",
            server_url: "http://test.local",
            repo_name: "test-repo",
            vai_api_key: "test-token",
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn vault_key_injected_into_worker_env() {
        let repo_id = Uuid::new_v4();
        let secrets = MockSecretsStore::with_key("vault-key-123");
        let workers = MockWorkerStore::new(true, 0);
        let compute = MockCompute::new();

        let result = spawn_if_capacity(
            &repo_id,
            compute.as_ref(),
            workers.clone(),
            secrets,
            "",
            &test_config(),
        )
        .await;

        assert!(result.unwrap().is_some(), "worker should have been spawned");
        assert!(compute.was_spawned(), "compute provider should have been called");
    }

    #[tokio::test]
    async fn fallback_key_used_when_vault_empty() {
        let repo_id = Uuid::new_v4();
        let secrets = MockSecretsStore::empty();
        let workers = MockWorkerStore::new(true, 0);
        let compute = MockCompute::new();

        let result = spawn_if_capacity(
            &repo_id,
            compute.as_ref(),
            workers.clone(),
            secrets,
            "fallback-key",
            &test_config(),
        )
        .await;

        assert!(result.unwrap().is_some(), "should spawn using fallback key");
        assert!(compute.was_spawned());
    }

    #[tokio::test]
    async fn missing_key_returns_error_no_machine_spawned() {
        let repo_id = Uuid::new_v4();
        let secrets = MockSecretsStore::empty();
        let workers = MockWorkerStore::new(true, 0);
        let compute = MockCompute::new();

        let result = spawn_if_capacity(
            &repo_id,
            compute.as_ref(),
            workers.clone(),
            secrets,
            "",
            &test_config(),
        )
        .await;

        assert!(
            matches!(result, Err(RegistryError::MissingRepoSecret("ANTHROPIC_API_KEY"))),
            "expected MissingRepoSecret error"
        );
        assert!(!compute.was_spawned(), "no machine should be booted when key is missing");
        assert!(
            workers.spawned.lock().await.is_empty(),
            "no worker row should be created when key is missing"
        );
    }

    #[tokio::test]
    async fn cloud_disabled_returns_none() {
        let repo_id = Uuid::new_v4();
        let secrets = MockSecretsStore::with_key("some-key");
        let workers = MockWorkerStore::new(false, 0);
        let compute = MockCompute::new();

        let result = spawn_if_capacity(
            &repo_id,
            compute.as_ref(),
            workers,
            secrets,
            "",
            &test_config(),
        )
        .await;

        assert!(result.unwrap().is_none());
        assert!(!compute.was_spawned());
    }

    #[tokio::test]
    async fn all_vault_secrets_injected_into_worker_env() {
        // Verifies that DATABASE_URL, BETTER_AUTH_SECRET, etc. from the vault
        // are all injected so dev servers that need them can start.
        let repo_id = Uuid::new_v4();
        let secrets = MockSecretsStore::with_secrets(&[
            ("ANTHROPIC_API_KEY", "vault-key"),
            ("DATABASE_URL", "postgres://test/db"),
            ("BETTER_AUTH_SECRET", "s3cr3t"),
        ]);
        let workers = MockWorkerStore::new(true, 0);
        let compute = MockCompute::new();

        let result = spawn_if_capacity(
            &repo_id,
            compute.as_ref(),
            workers.clone(),
            secrets,
            "",
            &test_config(),
        )
        .await;

        assert!(result.unwrap().is_some(), "worker should have been spawned");
        assert!(compute.was_spawned());
    }

    #[tokio::test]
    async fn concurrency_cap_returns_none() {
        let repo_id = Uuid::new_v4();
        let secrets = MockSecretsStore::with_key("some-key");
        let workers = MockWorkerStore::new(true, DEFAULT_MAX_CONCURRENT);
        let compute = MockCompute::new();

        let result = spawn_if_capacity(
            &repo_id,
            compute.as_ref(),
            workers,
            secrets,
            "",
            &test_config(),
        )
        .await;

        assert!(result.unwrap().is_none());
        assert!(!compute.was_spawned());
    }

    #[test]
    fn configured_max_concurrent_defaults_to_constant() {
        // Remove the env var in case a prior test set it.
        std::env::remove_var("VAI_WORKER_MAX_CONCURRENT");
        assert_eq!(configured_max_concurrent(), DEFAULT_MAX_CONCURRENT);
    }

    #[test]
    fn configured_max_concurrent_reads_env_var() {
        // Safety: this test mutates an env var.  Tests run in separate threads
        // so we scope the mutation tightly with a restore.
        std::env::set_var("VAI_WORKER_MAX_CONCURRENT", "7");
        let result = configured_max_concurrent();
        std::env::remove_var("VAI_WORKER_MAX_CONCURRENT");
        assert_eq!(result, 7);
    }
}
