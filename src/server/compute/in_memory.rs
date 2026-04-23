//! In-memory [`ComputeProvider`] adapter for deterministic testing.
//!
//! Workers are stored in a `Mutex`-protected map; their state can be advanced
//! manually with [`InMemoryProvider::set_state`] to simulate lifecycle
//! transitions without any network calls.
//!
//! # Example
//!
//! ```rust,no_run
//! # use std::collections::HashMap;
//! # use vai::server::compute::in_memory::InMemoryProvider;
//! # use vai::server::compute::{ComputeProvider, ResourceClass, WorkerSpec, WorkerState};
//! # async fn example() {
//! let provider = InMemoryProvider::new();
//! let spec = WorkerSpec {
//!     image: "ghcr.io/jjordy/vai-worker:test".into(),
//!     env: HashMap::new(),
//!     resources: ResourceClass::Small,
//!     labels: HashMap::new(),
//!     idempotency_key: "idem-1".into(),
//! };
//! let id = provider.spawn(spec).await.unwrap();
//! provider.set_state(&id, WorkerState::Running);
//! let status = provider.describe(&id).await.unwrap();
//! assert_eq!(status.state, WorkerState::Running);
//! provider.destroy(&id).await.unwrap();
//! assert!(provider.describe(&id).await.is_err());
//! # }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use uuid::Uuid;

use super::{
    ComputeProvider, MachineId, ProviderError, WorkerLabels, WorkerSpec, WorkerState, WorkerStatus,
    WorkerSummary,
};

// ── Internal record ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct WorkerRecord {
    spec: WorkerSpec,
    state: WorkerState,
    exit_code: Option<i32>,
}

// ── Provider ──────────────────────────────────────────────────────────────────

/// In-memory compute provider for unit and integration tests.
///
/// All state is held in a `Mutex`-protected map keyed by [`MachineId`].
/// Spawn assigns a fresh UUID as the machine id; state transitions are driven
/// by [`Self::set_state`] rather than an external event source.
#[derive(Debug, Default)]
pub struct InMemoryProvider {
    workers: Arc<Mutex<HashMap<MachineId, WorkerRecord>>>,
}

impl InMemoryProvider {
    /// Create a new empty provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Manually advance the state of a worker.
    ///
    /// Silently does nothing if `id` is not known (the worker may have been
    /// destroyed already).
    pub fn set_state(&self, id: &MachineId, state: WorkerState) {
        let mut map = self.workers.lock().expect("in-memory provider lock poisoned");
        if let Some(record) = map.get_mut(id) {
            record.state = state;
        }
    }

    /// Set the exit code for a worker (used alongside terminal states).
    pub fn set_exit_code(&self, id: &MachineId, exit_code: i32) {
        let mut map = self.workers.lock().expect("in-memory provider lock poisoned");
        if let Some(record) = map.get_mut(id) {
            record.exit_code = Some(exit_code);
        }
    }

    /// Return the number of workers currently tracked.
    pub fn worker_count(&self) -> usize {
        self.workers.lock().expect("in-memory provider lock poisoned").len()
    }

    /// Return the environment variables from the spawn spec of `id`, if known.
    ///
    /// Used in tests to verify that the correct API keys were injected.
    pub fn get_worker_env(&self, id: &MachineId) -> Option<HashMap<String, String>> {
        let map = self.workers.lock().expect("in-memory provider lock poisoned");
        map.get(id).map(|r| r.spec.env.clone())
    }
}

#[async_trait]
impl ComputeProvider for InMemoryProvider {
    async fn spawn(&self, spec: WorkerSpec) -> Result<MachineId, ProviderError> {
        let id = MachineId(Uuid::new_v4().to_string());
        let record = WorkerRecord {
            spec,
            state: WorkerState::Spawning,
            exit_code: None,
        };
        self.workers
            .lock()
            .expect("in-memory provider lock poisoned")
            .insert(id.clone(), record);
        Ok(id)
    }

    async fn destroy(&self, id: &MachineId) -> Result<(), ProviderError> {
        let removed = self
            .workers
            .lock()
            .expect("in-memory provider lock poisoned")
            .remove(id);
        if removed.is_some() {
            Ok(())
        } else {
            Err(ProviderError::NotFound(id.clone()))
        }
    }

    async fn describe(&self, id: &MachineId) -> Result<WorkerStatus, ProviderError> {
        let map = self.workers.lock().expect("in-memory provider lock poisoned");
        let record = map.get(id).ok_or_else(|| ProviderError::NotFound(id.clone()))?;
        Ok(WorkerStatus {
            id: id.clone(),
            state: record.state.clone(),
            exit_code: record.exit_code,
        })
    }

    async fn list(&self, labels: &WorkerLabels) -> Result<Vec<WorkerSummary>, ProviderError> {
        let map = self.workers.lock().expect("in-memory provider lock poisoned");
        let summaries = map
            .iter()
            .filter(|(_, record)| {
                labels.iter().all(|(k, v)| record.spec.labels.get(k) == Some(v))
            })
            .map(|(id, record)| WorkerSummary {
                id: id.clone(),
                state: record.state.clone(),
                labels: record.spec.labels.clone(),
            })
            .collect();
        Ok(summaries)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_spec(idempotency_key: &str) -> WorkerSpec {
        WorkerSpec {
            image: "ghcr.io/jjordy/vai-worker:test".into(),
            env: HashMap::new(),
            resources: super::super::ResourceClass::Small,
            labels: HashMap::new(),
            idempotency_key: idempotency_key.into(),
        }
    }

    #[tokio::test]
    async fn spawn_describe_running_destroy() {
        let provider = InMemoryProvider::new();
        let id = provider.spawn(test_spec("k1")).await.unwrap();

        // Freshly spawned workers start in Spawning state.
        let status = provider.describe(&id).await.unwrap();
        assert_eq!(status.state, WorkerState::Spawning);

        // Advance to Running.
        provider.set_state(&id, WorkerState::Running);
        let status = provider.describe(&id).await.unwrap();
        assert_eq!(status.state, WorkerState::Running);
        assert_eq!(status.id, id);

        // Destroy removes the worker.
        provider.destroy(&id).await.unwrap();
        let err = provider.describe(&id).await.unwrap_err();
        assert!(matches!(err, ProviderError::NotFound(_)));
    }

    #[tokio::test]
    async fn destroy_nonexistent_returns_not_found() {
        let provider = InMemoryProvider::new();
        let fake_id = MachineId("does-not-exist".into());
        let err = provider.destroy(&fake_id).await.unwrap_err();
        assert!(matches!(err, ProviderError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_filters_by_labels() {
        let provider = InMemoryProvider::new();

        let mut spec_a = test_spec("k-a");
        spec_a.labels.insert("repo".into(), "alpha".into());
        let id_a = provider.spawn(spec_a).await.unwrap();

        let mut spec_b = test_spec("k-b");
        spec_b.labels.insert("repo".into(), "beta".into());
        provider.spawn(spec_b).await.unwrap();

        // Filter for repo=alpha — should return only id_a.
        let mut filter = WorkerLabels::new();
        filter.insert("repo".into(), "alpha".into());
        let results = provider.list(&filter).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id_a);

        // Empty filter returns all workers.
        let all = provider.list(&WorkerLabels::new()).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn spawn_increments_worker_count() {
        let provider = InMemoryProvider::new();
        assert_eq!(provider.worker_count(), 0);
        provider.spawn(test_spec("k1")).await.unwrap();
        assert_eq!(provider.worker_count(), 1);
        provider.spawn(test_spec("k2")).await.unwrap();
        assert_eq!(provider.worker_count(), 2);
    }

    #[tokio::test]
    async fn completed_worker_has_exit_code() {
        let provider = InMemoryProvider::new();
        let id = provider.spawn(test_spec("k1")).await.unwrap();
        provider.set_state(&id, WorkerState::Completed);
        provider.set_exit_code(&id, 0);
        let status = provider.describe(&id).await.unwrap();
        assert_eq!(status.state, WorkerState::Completed);
        assert_eq!(status.exit_code, Some(0));
    }
}
