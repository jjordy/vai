# PRD 28: Cloud Agent Runtime

## Summary

Spawn stateless per-issue agent workers on vai's managed cloud runtime when qualifying issues land in the queue. Moves the AI dev loop off the user's laptop and into a cloud worker pool. Managed compute + BYO Anthropic token. Server-only feature — local RALPH loops continue unchanged.

The compute runtime is abstracted behind a **`ComputeProvider` port**; Fly Machines is the MVP adapter. Swapping to AWS Fargate, GCP Cloud Run, or a self-hosted Nomad pool later is an adapter swap, not a rewrite.

This is the first feature that changes vai's pricing surface from "free tool" to "SaaS." It gates around Better Auth Organizations (subscriptions attach to orgs, not users) so Team tier is meaningful from day one even with solo users.

## Motivation

The current RALPH loop requires a terminal open on the user's machine per repo. That caps vai's core value proposition — AI-accelerated development — at "how many terminals can the user keep open." The laptop-as-bottleneck contradicts the pitch: if AI agents are the workforce, they should run 24/7 in the cloud.

The cloud runtime also unlocks:
- **Parallel execution** beyond what a dev laptop can sustain
- **Continuous operation** (agents keep working while the user sleeps)
- **Standardized environment** (canonical toolchain image vs "whatever I have installed locally")

## Non-goals

- Replacing GitHub Actions for deploy pipelines (use push-to-GitHub for that separately)
- Per-repo custom Dockerfile builds (future extension; MVP uses canonical image only)
- Self-serve Stripe billing (Phase 5; MVP is manual plan assignment)
- Supporting exotic toolchains not in the canonical image (fall back to local loops)

## Design decisions

| # | Decision | Rationale |
|---|---|---|
| 1 | **Per-issue stateless workers** | Matches existing `claim → submit → die` loop shape; crash recovery is free |
| 2 | **Pluggable compute runtime via `ComputeProvider` port; Fly Machines as MVP adapter** | vai-server already on Fly (~2s cold start, per-second billing), but the abstraction preserves vendor optionality. Future AWS/GCP adapters are net-new modules, not a rewrite |
| 3 | **vai-hosted compute, BYO Anthropic tokens** | Compute is ~$0.0025/issue; user's LLM spend is 100-1000× that |
| 4 | **Tiered concurrency (Free 3 / Pro 5 / Team 10)** | Single-worker cap would defeat the "faster than local" pitch |
| 5 | **Direct spawn from vai-server** (monolithic) | Don't pre-split; extract later if metrics demand |
| 6 | **Per-repo concurrency cap with queue fallback** | If cap hit, issues just stay `Open`; existing worker picks them up next iteration |
| 7 | **Server-only; local agents unchanged** | No forced migration; both modes coexist |
| 8 | **Canonical worker image only** (no per-repo Dockerfile builds) | Avoids builder/registry/cache subsystem; ~1 GB fat image covers 80%+ of use cases |
| 9 | **Verify runs natively inside the worker** | Quality is load-bearing. Canonical image bundles toolchains so `[checks]` run directly. No DinD, no second machine |
| 10 | **Playwright + Chromium pre-installed** | Enables E2E-verify flows (vai-dashboard pattern). ~500 MB image cost; Fly caches per machine family |
| 11 | **Manual plan assignment, no Stripe (MVP)** | One user for the foreseeable future. Admin CLI. Stripe deferred |
| 12 | **Subscriptions attach to Better Auth Organizations** | Team tier is meaningful from day one; solo users get a personal org auto-created |

## Architecture

```
┌──────────────────┐    1. POST /issues      ┌───────────────────────┐
│ User / Dashboard │───────────────────────▶ │    vai-server         │
└──────────────────┘                         │ ┌─────────────────┐   │
                                             │ │ issue handler   │   │
                                             │ └──────┬──────────┘   │
                                             │        │ 2. spawn_if_capacity()
                                             │        ▼              │
                                             │ ┌─────────────────┐   │    3. ComputeProvider.spawn()
                                             │ │ worker_registry │───┼─────────────────▶ ┌──────────────────────┐
                                             │ └────┬────────────┘   │  (Fly adapter MVP) │ vai-worker:vX.Y.Z    │
                                             │      ▼                │                    │ (cloud worker)       │
                                             │ ┌─────────────────┐   │   4. vai agent     │                      │
                                             │ │ agent_workers   │   │      claim         │ loop.sh:             │
                                             │ └─────────────────┘   │◀───────────────────│   - claim            │
                                             │                       │                    │   - download         │
                                             │                       │   5. Log stream    │   - claude -p        │
                                             │                       │◀───────────────────│   - vai agent verify │
                                             │                       │                    │   - submit           │
                                             └───────────────────────┘                    └──────────────────────┘
                                                     ▲                                            │
                                                     │    6. worker terminates (provider reaps)  │
                                                     └────────────────────────────────────────────┘
                                                       (cron reconciles orphaned state)
```

### Ports & adapters

```rust
// Port (infra-agnostic)
#[async_trait]
pub trait ComputeProvider: Send + Sync {
    async fn spawn(&self, spec: WorkerSpec) -> Result<MachineId, ProviderError>;
    async fn destroy(&self, id: &MachineId) -> Result<(), ProviderError>;
    async fn describe(&self, id: &MachineId) -> Result<WorkerStatus, ProviderError>;
    async fn list(&self, labels: &WorkerLabels) -> Result<Vec<WorkerSummary>, ProviderError>;
}

// Adapters
pub struct FlyMachinesProvider { /* API client, app_name, token */ }  // MVP
pub struct InMemoryProvider    { /* for tests */ }
// Future: AwsFargateProvider, CloudRunProvider, NomadProvider
```

The `MachineId` is an opaque newtype (UUID string) that's provider-interpreted. The port exposes only what `worker_registry` actually needs; we do **not** leak provider-specific concepts (Fly regions, machine classes, etc.) through the port. Provider-specific config lives in the adapter constructor and can be TOML-driven later.

### Data flow (happy path)

1. User creates issue via dashboard or CLI
2. vai-server calls `worker_registry::spawn_if_capacity(repo_id)`:
   - `repo.cloud_agent_enabled = true`?
   - `count(agent_workers WHERE repo_id=? AND state='running') < org.plan.max_concurrent`?
   - `repo_agent_secrets` has `ANTHROPIC_API_KEY`?
3. Call `ComputeProvider::spawn(spec)` with canonical image + env vars (Anthropic key, repo slug, 1-hour vai API key). The injected provider (Fly adapter in prod) makes the actual API call.
4. Worker runs standard RALPH cycle
5. Worker POSTs stdout/stderr to `/api/worker-logs/:worker_id` every ~5s
6. On exit, worker POSTs final state; vai-server marks `completed`/`failed`
7. Cron (~60s) reconciles dead workers; requeues issues if workspace still claimed

### Invariants

- **Exactly-once NOT required.** Dead worker → issue back to `Open` → another worker claims. Server-side claim is atomic.
- **Secrets never hit disk on the worker.** Env vars only; ephemeral vai API key is repo-scoped, 1-hour TTL.
- **Workers are disposable.** No inter-run state.

## Canonical worker image

**Tag:** `ghcr.io/jjordy/vai-worker:v<vai-version>`, multi-arch (arm64 + amd64). Published to GHCR (a vendor-neutral OCI registry) so any compute provider can pull it — no Fly-specific registry coupling.

Layered for cache reuse:

1. **Base** — `debian:bookworm-slim` + git, curl, jq, ripgrep, ca-certificates, openssh-client
2. **Toolchain** — Node 22 + pnpm, Rust stable + cargo, Python 3 + pip, `mise` for version pinning
3. **Browser** — Chromium + Playwright system deps
4. **AI tools** — claude CLI, anthropic SDK (pinned)
5. **vai CLI** — `COPY` from release artifact
6. **Entrypoint** — `loop.sh`, scripts

Estimated size: ~1 GB. Fly caches per machine family → cold start ~2-3s after first spawn.

Users' `[checks]` run directly (no nested container). `mise` reads `.tool-versions` for repos needing specific versions.

## Schema

### Migration 1 — `20260424000001_cloud_agent_runtime.sql`

```sql
CREATE TABLE plans (
  tier TEXT PRIMARY KEY,
  max_concurrent_workers_per_repo INT NOT NULL,
  max_issues_per_month INT,
  log_retention_days INT NOT NULL
);

ALTER TABLE repos ADD COLUMN cloud_agent_enabled BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE repo_agent_secrets (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  key TEXT NOT NULL,
  encrypted_value BYTEA NOT NULL,
  nonce BYTEA NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE (repo_id, key)
);

CREATE TABLE agent_workers (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  provider TEXT NOT NULL DEFAULT 'fly',           -- 'fly' | 'aws_fargate' | 'gcp_cloud_run' | ...
  machine_id TEXT,                                -- opaque ID, provider-interpreted
  state TEXT NOT NULL CHECK (state IN ('spawning','running','completed','failed','dead')),
  workspace_id UUID REFERENCES workspaces(id),
  last_heartbeat_at TIMESTAMPTZ,
  started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  ended_at TIMESTAMPTZ
);
CREATE INDEX idx_agent_workers_repo_state ON agent_workers(repo_id, state);

CREATE TABLE agent_worker_logs (
  id BIGSERIAL PRIMARY KEY,
  worker_id UUID NOT NULL REFERENCES agent_workers(id) ON DELETE CASCADE,
  ts TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  stream TEXT NOT NULL CHECK (stream IN ('stdout','stderr')),
  chunk TEXT NOT NULL
);
CREATE INDEX idx_agent_worker_logs_worker ON agent_worker_logs(worker_id, ts);

INSERT INTO plans (tier, max_concurrent_workers_per_repo, max_issues_per_month, log_retention_days) VALUES
  ('free', 3, 100, 1),
  ('pro',  5, NULL, 30),
  ('team', 10, NULL, 90);
```

### Migration 2 (Phase 4) — Better Auth `organization` tables

Generated by `npx @better-auth/cli migrate` after adding the plugin.

### Migration 3 (Phase 4) — `org_subscriptions` + `repos.org_id`

```sql
CREATE TABLE org_subscriptions (
  org_id UUID PRIMARY KEY REFERENCES organization(id),
  plan_tier TEXT NOT NULL REFERENCES plans(tier) DEFAULT 'free',
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE repos ADD COLUMN org_id UUID REFERENCES organization(id);
-- Data migration: personal org per user + attach existing repos
```

## Files to build on

Existing (reused as-is):
- `src/agent/mod.rs` — claim/download/submit primitives (stateless-friendly)
- `src/cli/agent_loop/templates/Dockerfile.claude-code` — starting point for cloud-worker Dockerfile
- `src/cli/agent_loop/templates/loop.sh` — starting point for worker entrypoint
- `src/server/work_queue.rs` — atomic claim endpoint
- `src/event_log/mod.rs` — `IssueCreated` event kind (trigger source)
- `src/server/issue.rs` — issue-creation handler (add `spawn_if_capacity` call)

New modules:
- `src/server/worker_registry.rs` — spawn/reconcile/capacity queries; depends only on the `ComputeProvider` trait, never a concrete adapter
- `src/server/compute/mod.rs` — `ComputeProvider` trait + shared types (`WorkerSpec`, `MachineId`, `WorkerStatus`, `ProviderError`)
- `src/server/compute/fly.rs` — `FlyMachinesProvider` adapter (MVP)
- `src/server/compute/in_memory.rs` — test adapter
- `src/server/secrets.rs` — AES-GCM wrap/unwrap
- `src/server/worker_logs.rs` — log ingest + retention cron

Infra:
- `fly.toml` — add secret for the Fly adapter's API token (e.g. `VAI_COMPUTE_FLY_TOKEN`) — namespaced so future providers get their own
- `.github/workflows/worker-image.yml` — build/push canonical image to GHCR on vai release tags
- `docker/worker/Dockerfile` — canonical image

Server config (future-facing, even if Fly is the only adapter today):

```toml
[compute]
provider = "fly"

[compute.fly]
app_name = "vai-workers"
region = "iad"
# token read from env: VAI_COMPUTE_FLY_TOKEN

# (example placeholder for future providers)
# [compute.aws_fargate]
# cluster = "vai-workers"
# task_role_arn = "..."
```

## Phased delivery

**Phase 1 — Backend runtime** (multi-PR)
Migration 1, `ComputeProvider` trait + Fly adapter + in-memory test adapter, `worker_registry`/`secrets`/`worker_logs` modules, `spawn_if_capacity` hook, dead-worker cron, integration tests against the in-memory adapter.

**Phase 2 — Canonical worker image**
`docker/worker/Dockerfile` with all layers, `loop.sh` with heartbeat POSTs, GitHub Actions publish on release, smoke test.

**Phase 3 — Dashboard UI**
Repo settings (toggle + token form), worker activity view with live logs, plan status widget.

**Phase 4 — Better Auth Organizations + billing plumbing**
BA organization plugin, Migration 2 + 3, data migration (personal orgs for existing users), capacity check reads from `org_subscriptions`, admin CLI for plan changes.

**Phase 5 (future) — Self-serve billing**
Stripe checkout, webhook sync, upgrade UI.

## Verification

1. **Phase 1:** `cargo test --features full` + integration tests for `spawn_if_capacity` state machine, driven through the `InMemoryProvider` adapter (no network, no Fly dependency)
2. **Phase 1 E2E:** Deploy to staging with the Fly adapter; create issue in cloud-enabled repo; observe worker lifecycle through both vai-server APIs and provider-native tools (e.g. `fly machine list` for the Fly adapter)
3. **Phase 2:** Manual `docker buildx build --platform linux/amd64,linux/arm64`; run against staging with prod-like env; complete one claim/verify/submit cycle
4. **Phase 2 canaries:** vai-dashboard `pnpm typecheck && pnpm test && playwright test` green in the canonical image. vai: `cargo clippy && cargo test`.
5. **Phase 3:** Playwright E2E: enable cloud mode in UI → create issue → watch live log → see auto-resolve
6. **Phase 4:** New user → personal org auto-created → repos attach → plan cap enforced per org

## Open risks

- **Fly org quota ceiling** — per-org concurrent machine limits. Year-out. Mitigation: request quota bump, shard across Fly orgs, or add a second `ComputeProvider` adapter to distribute load.
- **Provider-specific concepts leaking through the port** — if a future requirement (e.g. GPU workers) can only be expressed in Fly terms, the `WorkerSpec` type could bend toward Fly. Mitigation: keep `WorkerSpec` minimal (image, env, resources); push provider-specific knobs into adapter-level config, not the port.
- **Log volume cost** — Postgres storage for logs. Move to R2 if any user exceeds ~10 MB/month.
- **Worker image staleness** — CI must publish on every vai release. Mitigation: `loop.sh` checks `vai --version` matches expected.
- **Verify toolchain drift** — canonical Node 22 / Rust stable / Python 3.12 may not match repo's declared versions. Mitigation: `mise` reads `.tool-versions`.
- **BA Organizations migration** — data migration of existing repos → orgs must be atomic. Rehearse against staging before prod cutover.
