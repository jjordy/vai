//! Fly Machines adapter for the [`ComputeProvider`] port (PRD 28).
//!
//! Spawns per-issue agent workers as Fly Machines in the configured app.
//! Authentication uses the `VAI_COMPUTE_FLY_TOKEN` environment variable (set
//! as a Fly secret on the vai-server app).

use std::collections::HashMap;

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    ComputeProvider, MachineId, ProviderError, ResourceClass, WorkerLabels, WorkerSpec,
    WorkerState, WorkerStatus, WorkerSummary,
};

const FLY_API_BASE: &str = "https://api.machines.dev/v1";

/// Fly Machines implementation of [`ComputeProvider`].
///
/// Requires `VAI_COMPUTE_FLY_TOKEN` to be set in the environment.
/// Workers are spawned in the configured `app_name` Fly app.
pub struct FlyMachinesProvider {
    app_name: String,
    region: String,
    token: String,
    client: reqwest::Client,
}

impl FlyMachinesProvider {
    /// Build a provider from environment variables.
    ///
    /// Reads `VAI_COMPUTE_FLY_TOKEN` for the API token.
    /// `app_name` is the Fly app that will host worker machines.
    /// `region` is the preferred spawn region (e.g. `"iad"`).
    pub fn from_env(app_name: impl Into<String>, region: impl Into<String>) -> Option<Self> {
        let token = std::env::var("VAI_COMPUTE_FLY_TOKEN").ok()?;
        Some(Self {
            app_name: app_name.into(),
            region: region.into(),
            token,
            client: reqwest::Client::new(),
        })
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    fn machines_url(&self) -> String {
        format!("{FLY_API_BASE}/apps/{}/machines", self.app_name)
    }

    fn machine_url(&self, id: &str) -> String {
        format!("{FLY_API_BASE}/apps/{}/machines/{id}", self.app_name)
    }
}

// ── Fly API response types ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FlyMachine {
    id: String,
    state: String,
}

fn fly_state_to_worker_state(s: &str) -> WorkerState {
    match s {
        "created" | "starting" => WorkerState::Spawning,
        "started" => WorkerState::Running,
        "stopped" => WorkerState::Completed,
        "failed" => WorkerState::Failed,
        _ => WorkerState::Dead,
    }
}

fn resource_class_to_guest(class: ResourceClass) -> serde_json::Value {
    match class {
        ResourceClass::Small => serde_json::json!({ "cpu_kind": "shared", "cpus": 1, "memory_mb": 512 }),
        ResourceClass::Medium => serde_json::json!({ "cpu_kind": "shared", "cpus": 2, "memory_mb": 1024 }),
        ResourceClass::Large => serde_json::json!({ "cpu_kind": "shared", "cpus": 4, "memory_mb": 2048 }),
    }
}

#[async_trait]
impl ComputeProvider for FlyMachinesProvider {
    async fn spawn(&self, spec: WorkerSpec) -> Result<MachineId, ProviderError> {
        let body = serde_json::json!({
            "config": {
                "image": spec.image,
                "env": spec.env,
                "auto_destroy": true,
                "restart": { "policy": "no" },
                "guest": resource_class_to_guest(spec.resources),
            },
            "region": self.region,
            "name": format!("vai-worker-{}", &spec.idempotency_key[..8.min(spec.idempotency_key.len())]),
        });

        let resp = self
            .client
            .post(self.machines_url())
            .header("Authorization", self.auth_header())
            .header("Idempotency-Key", &spec.idempotency_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transient(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Provider(format!("{status}: {text}")));
        }

        let machine: FlyMachine = resp
            .json()
            .await
            .map_err(|e| ProviderError::Transient(e.to_string()))?;

        Ok(MachineId(machine.id))
    }

    async fn destroy(&self, id: &MachineId) -> Result<(), ProviderError> {
        let url = format!("{}?force=true", self.machine_url(&id.0));
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| ProviderError::Transient(e.to_string()))?;

        match resp.status().as_u16() {
            200 | 204 | 404 => Ok(()),
            s => {
                let text = resp.text().await.unwrap_or_default();
                Err(ProviderError::Provider(format!("{s}: {text}")))
            }
        }
    }

    async fn describe(&self, id: &MachineId) -> Result<WorkerStatus, ProviderError> {
        let resp = self
            .client
            .get(self.machine_url(&id.0))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| ProviderError::Transient(e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Err(ProviderError::NotFound(id.clone()));
        }

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Provider(text));
        }

        let machine: FlyMachine = resp
            .json()
            .await
            .map_err(|e| ProviderError::Transient(e.to_string()))?;

        Ok(WorkerStatus {
            id: id.clone(),
            state: fly_state_to_worker_state(&machine.state),
            exit_code: None,
        })
    }

    async fn list(&self, labels: &WorkerLabels) -> Result<Vec<WorkerSummary>, ProviderError> {
        let resp = self
            .client
            .get(self.machines_url())
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| ProviderError::Transient(e.to_string()))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Provider(text));
        }

        let machines: Vec<FlyMachine> = resp
            .json()
            .await
            .map_err(|e| ProviderError::Transient(e.to_string()))?;

        let summaries = machines
            .into_iter()
            .filter(|_| labels.is_empty())
            .map(|m| WorkerSummary {
                id: MachineId(m.id),
                state: fly_state_to_worker_state(&m.state),
                labels: HashMap::new(),
            })
            .collect();

        Ok(summaries)
    }
}
