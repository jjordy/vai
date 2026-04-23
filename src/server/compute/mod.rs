//! Compute provider port for the cloud agent runtime (PRD 28).
//!
//! Defines the [`ComputeProvider`] trait and the shared types it operates on.
//! Concrete adapters live in sub-modules:
//!
//! - [`in_memory`] — deterministic test adapter (no network)
//! - `fly` (future) — Fly Machines adapter (MVP production adapter)
//!
//! No adapter-specific concepts (Fly regions, machine classes, etc.) are
//! allowed in this module. Provider-specific configuration lives entirely
//! inside adapter constructors.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod in_memory;

// ── Types ─────────────────────────────────────────────────────────────────────

/// Opaque provider-assigned identifier for a spawned worker machine.
///
/// The string value is interpreted entirely by the provider that created it.
/// Consumers must not parse or construct `MachineId` values directly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MachineId(pub String);

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// CPU and memory resource class for a worker.
///
/// Maps to provider-specific sizes inside each adapter.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceClass {
    /// Minimal resources — suitable for lightweight agent loops.
    Small,
    /// Standard resources — default for most workloads.
    #[default]
    Medium,
    /// High-memory resources — large codebases or parallel tool calls.
    Large,
}

/// Key-value labels attached to a worker for filtering via [`ComputeProvider::list`].
pub type WorkerLabels = HashMap<String, String>;

/// Specification for a worker to be spawned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSpec {
    /// OCI image reference (e.g. `ghcr.io/jjordy/vai-worker:v1.2.3`).
    pub image: String,
    /// Environment variables injected into the worker.
    pub env: HashMap<String, String>,
    /// CPU / memory resource hint.
    pub resources: ResourceClass,
    /// Labels used for filtering when calling [`ComputeProvider::list`].
    pub labels: WorkerLabels,
    /// Provider-level idempotency key — prevents double-spawn on retried calls.
    pub idempotency_key: String,
}

/// Observed lifecycle state of a running worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerState {
    /// Worker has been requested but may not have started yet.
    Spawning,
    /// Worker is executing the agent loop.
    Running,
    /// Worker exited successfully.
    Completed,
    /// Worker exited with a non-zero status.
    Failed,
    /// Worker is no longer reachable and presumed dead.
    Dead,
}

/// Detailed status of a single worker returned by [`ComputeProvider::describe`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub id: MachineId,
    pub state: WorkerState,
    /// Exit code, if the worker has terminated.
    pub exit_code: Option<i32>,
}

/// Brief summary of a worker returned by [`ComputeProvider::list`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSummary {
    pub id: MachineId,
    pub state: WorkerState,
    pub labels: WorkerLabels,
}

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors returned by [`ComputeProvider`] operations.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The requested machine does not exist.
    #[error("machine not found: {0}")]
    NotFound(MachineId),

    /// The provider rejected the request (e.g. quota exceeded, auth failure).
    #[error("provider error: {0}")]
    Provider(String),

    /// A transient network or I/O failure — callers may retry.
    #[error("transient error: {0}")]
    Transient(String),
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Abstracts the compute runtime used to spawn per-issue agent workers.
///
/// Implementations must be `Send + Sync` so they can be stored in `Arc<dyn
/// ComputeProvider>` and shared across Tokio tasks without additional locking.
///
/// The trait is deliberately narrow — it exposes only the operations that
/// `worker_registry` actually needs. Provider-specific capabilities (e.g. GPU
/// workers, persistent volumes) are not part of the port.
#[async_trait]
pub trait ComputeProvider: Send + Sync {
    /// Spawn a new worker with the given specification.
    ///
    /// Returns the opaque [`MachineId`] assigned by the provider. Callers
    /// should store this id in `agent_workers.machine_id` for later lifecycle
    /// operations.
    ///
    /// Using `spec.idempotency_key` prevents double-spawn when the caller
    /// retries after a transient failure.
    async fn spawn(&self, spec: WorkerSpec) -> Result<MachineId, ProviderError>;

    /// Destroy the worker identified by `id`.
    ///
    /// Returns `Ok(())` if the worker was destroyed or was already gone.
    /// Returns [`ProviderError::NotFound`] if the id is unknown.
    async fn destroy(&self, id: &MachineId) -> Result<(), ProviderError>;

    /// Return the current status of the worker identified by `id`.
    ///
    /// Returns [`ProviderError::NotFound`] if the id is unknown.
    async fn describe(&self, id: &MachineId) -> Result<WorkerStatus, ProviderError>;

    /// List workers whose labels are a superset of `labels`.
    ///
    /// An empty `labels` map returns all workers visible to this provider
    /// instance.
    async fn list(&self, labels: &WorkerLabels) -> Result<Vec<WorkerSummary>, ProviderError>;
}
