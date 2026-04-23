//! Worker registry — capacity checks, spawn orchestration, and dead-worker
//! reconciliation (PRD 28).
//!
//! The primary entry point is [`spawn_if_capacity`], called by the
//! issue-creation handler after a new issue is persisted.
//!
//! [`run_reconciliation_loop`] is a background task that periodically finds
//! stale workers (no heartbeat for `VAI_WORKER_STALE_SECS`, default 300 s)
//! and marks them dead, discarding their claimed workspaces so the linked
//! issues requeue automatically.

use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::storage::{NewWorker, WorkerDoneReason, WorkerStore};

use super::compute::{ComputeProvider, MachineId, WorkerSpec};

/// Hard-coded per-repo concurrency cap used until org-level plan billing lands
/// (PRD 28 Phase 4).  Mirrors the `free` tier in the `plans` table.
const DEFAULT_MAX_CONCURRENT: u64 = 3;

/// Configuration passed to [`spawn_if_capacity`].
pub struct SpawnConfig<'a> {
    /// OCI image reference for the canonical worker (e.g. `ghcr.io/jjordy/vai-worker:v1.2.3`).
    pub worker_image: &'a str,
    /// Public URL of the vai server, injected as `VAI_SERVER_URL` in the worker.
    pub server_url: &'a str,
    /// Short-lived API key the worker will use to authenticate against this server.
    pub vai_api_key: &'a str,
    /// Anthropic API key injected as `ANTHROPIC_API_KEY` in the worker.
    pub anthropic_api_key: &'a str,
}

/// Errors from spawn operations.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),
    #[error("compute provider error: {0}")]
    Provider(String),
}

/// Attempt to spawn a cloud worker for `repo_id`.
///
/// Returns `Some(worker_id)` if a worker was spawned, `None` if the repo has
/// cloud agents disabled or is already at the concurrency cap.
///
/// The function is **fire-and-continue**: it inserts the `agent_workers` row
/// before the machine actually boots, so callers can return the new issue
/// immediately without blocking on provider latency.
pub async fn spawn_if_capacity(
    repo_id: &Uuid,
    compute: &dyn ComputeProvider,
    workers: Arc<dyn WorkerStore>,
    config: &SpawnConfig<'_>,
) -> Result<Option<Uuid>, RegistryError> {
    // Check cloud_agent_enabled on the repo.
    if !workers.is_cloud_agent_enabled(repo_id).await? {
        return Ok(None);
    }

    // Check concurrency cap.
    let running = workers.count_running_workers(repo_id).await?;
    if running >= DEFAULT_MAX_CONCURRENT {
        tracing::debug!(
            repo_id = %repo_id,
            running,
            cap = DEFAULT_MAX_CONCURRENT,
            "cloud agent concurrency cap reached — skipping spawn"
        );
        return Ok(None);
    }

    // Mint a unique idempotency key for this spawn attempt.
    let idempotency_key = Uuid::new_v4().to_string();

    // Build environment for the worker.
    let mut env = std::collections::HashMap::new();
    env.insert("VAI_SERVER_URL".into(), config.server_url.to_string());
    env.insert("VAI_API_KEY".into(), config.vai_api_key.to_string());
    env.insert("ANTHROPIC_API_KEY".into(), config.anthropic_api_key.to_string());
    env.insert("VAI_REPO_ID".into(), repo_id.to_string());

    let spec = WorkerSpec {
        image: config.worker_image.to_string(),
        env,
        resources: super::compute::ResourceClass::Medium,
        labels: {
            let mut l = std::collections::HashMap::new();
            l.insert("vai_repo_id".into(), repo_id.to_string());
            l
        },
        idempotency_key,
    };

    // Insert the worker row in 'spawning' state before calling the provider,
    // so a crash after spawn but before insert doesn't orphan a live machine.
    let worker = workers
        .create_worker(NewWorker {
            repo_id: *repo_id,
            provider: "fly".to_string(),
            machine_id: None,
        })
        .await?;

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
const DEFAULT_STALE_SECS: u32 = 300;

/// Reconcile one pass: find stale workers, discard their workspaces, reopen
/// their issues, and mark them dead.
///
/// Returns the number of workers marked dead.
async fn reconcile_once(storage: &crate::storage::StorageBackend, stale_secs: u32) -> u32 {
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

    marked
}

/// Spawn a background task that periodically reconciles dead workers.
///
/// Interval defaults to 60 s (`VAI_WORKER_RECONCILE_INTERVAL_SECS`) and stale
/// threshold defaults to 300 s (`VAI_WORKER_STALE_SECS`).
pub fn run_reconciliation_loop(storage: crate::storage::StorageBackend) {
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
            let n = reconcile_once(&storage, stale_secs).await;
            if n > 0 {
                tracing::info!(workers_reaped = n, "dead-worker reconciliation completed");
            }
        }
    });
}
