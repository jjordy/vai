# PRD 29: Cloud Agent Observability (Phase 3, Part 1)

**Status:** In progress — design captured from grill session 2026-04-24. Covers workspace-level agent panel and error classification. Real-time delivery, log volume handling, security/retention, and endpoint list are deferred to a follow-on PRD.

## Summary

Surface cloud agent activity where it's most useful: **on the workspace detail page** (in-context, state-aware) and at the **account level** (fleet-wide agent health). Errors get classified at the server, flow through the existing events table, and reach the UI in two partitions — workspace-scoped failures on the workspace page, account-scoped failures (Anthropic rate limits, auth, quota) in a top banner that doesn't multiply across affected workspaces.

## Motivation

PRD 28 Phase 2 smoke test proved end-to-end that cloud workers can claim, run claude, submit, and close issues. But when something goes wrong — claude hits a rate limit, a worker OOMs mid-run, verify fails after max attempts — the user currently has to SSH into the dashboard, query postgres, grep raw stdout chunks, and guess. That's not a product.

The value prop of cloud agents is "you scale your workflow without scaling your attention." Observability that doesn't match that scaling curve degrades fast. 5 workers failing for the same root cause should not produce 5 copies of the same error. 500 stdout chunks/minute from one chatty claude session shouldn't drown out the one line that says "my API key got revoked."

## Non-goals

- Full log management platform (no filtering DSL, no retention policies beyond plan-tier defaults, no export)
- Cross-repo / cross-user admin dashboards (org-wide observability comes later, after Better Auth orgs in PRD 28 Phase 4)
- Interactive agent steering (no "pause" / "redirect" of a running worker; that's a separate product capability)
- Metrics / billing surfaces (token usage graphs are Phase 5 billing work)

## Design decisions

These locked in during grill session 2026-04-24 (see memory `project_prd29_agent_observability_design.md` for context):

1. **Workspace page and repo-wide workers page are complementary, not duplicative.**
   Shared backend: one list endpoint filtered by `?workspace_id=` for the workspace panel, unfiltered for the repo page. Shared components: `WorkersTable`, `LogViewer`. Workspace panel is the natural in-context reading flow; repo page is the ops/monitoring view.

2. **Workspace agent panel is a single, state-aware adaptive component.**
   One inline section in the workspace detail page. Its content reshapes based on workspace state:
   - **Active + worker running** → live status header + phase + heartbeat age + autoscrolling log stream
   - **Failed / stuck** → timeline of state transitions + error prominence + full searchable log + retry controls
   - **Submitted / done** → collapsed summary ("completed at T, 15m total, [view logs]") with expandable retrospective
   - **No worker (human-created workspace)** → panel hidden entirely

   One component, one slot, multiple renderings. Matches GitHub's PR-status convention.

3. **Errors partition into Class A (account-level) and Class B (workspace-level); UI surfaces partition accordingly.**
   - **Class A**: Anthropic rate limit, auth failure, quota exceeded, service overload; Fly spawn-path failures (quota, auth) — all affect every worker for a given user simultaneously. Surface in a top banner (account-level) and in the plan-status widget. Coalesced — 12 workers with the same Anthropic 429 show as one banner: "Anthropic rate-limited, 12 workers affected."
   - **Class B**: verify failure after max attempts, submit conflict, download failure, worker OOM, `claude -p` protocol error — localized to one worker/workspace. Surface on the workspace page only.
   - Workspace panel cross-references when relevant: "blocked by account-level issue — see banner."

4. **Error classification is server-side, single source of truth; worker reports via `vai agent error` subcommand.**
   No raw HTTP from bash. loop.sh wraps `claude -p` and, on non-zero exit, pipes stderr to `vai agent error`. The CLI posts the raw stderr (plus phase + exit code) to `POST /api/agent-workers/:id/error`. The server's classifier module recognizes Anthropic error envelopes, Claude CLI output patterns, and infrastructure signatures, then persists an `EventKind::WorkerError` event and returns the classification to the caller. CLI exit code indicates transient (retry) vs fatal (abort). Optional `--wait` flag has the CLI block for `retry_after_seconds` before returning.

## Architecture

### `vai agent error` subcommand

```
vai agent error
  [--stderr-file PATH | stdin]
  [--exit-code N]
  [--phase claim|download|claude|verify|submit]
  [--wait]
  [--output-format json|pretty]
```

Inputs flow to `POST /api/agent-workers/:id/error`:

```jsonc
// Request
{
  "phase": "claude",                      // optional context
  "exit_code": 1,                         // optional
  "raw_stderr": "..."                     // bounded; server truncates at 64 KiB
}

// Response
{
  "category": "anthropic_rate_limit",     // WorkerErrorCategory enum value
  "transient": true,                      // should worker retry?
  "retry_after_seconds": 60,              // if present, CLI can --wait
  "human_message": "Anthropic API rate limit hit; retrying in 60 seconds",
  "severity": "warn"
}
```

CLI exit code:
- `0` — transient (retry); with `--wait` the CLI has already slept
- `1` — fatal (reset and abort this claim)
- `2` — server couldn't classify (`Uncategorized`); treat as fatal but log loudly

### Server classifier

New module `src/server/worker_error_classifier.rs` — single place patterns live. Structure:

```rust
struct ClassifierInput {
    raw_stderr: String,
    exit_code: Option<i32>,
    phase: Option<String>,
}

pub struct ClassifiedError {
    pub category: WorkerErrorCategory,
    pub transient: bool,
    pub retry_after_seconds: Option<u64>,
    pub human_message: String,
    pub severity: ErrorSeverity,
}

pub enum WorkerErrorCategory {
    AnthropicRateLimit,
    AnthropicAuthFailed,
    AnthropicQuotaExceeded,
    AnthropicOverloaded,
    AnthropicBadRequest,
    WorkerOom,
    WorkerSpawnFailed,
    VerifyFailed,
    SubmitConflict,
    DownloadFailed,
    ClaudeCliProtocol,
    Uncategorized(String),
}

pub enum ErrorSeverity { Info, Warn, Error }

pub fn classify(input: &ClassifierInput) -> ClassifiedError { /* ... */ }
```

Pattern rules are a list of `(regex | signature, -> ClassifiedError)` pairs. Ordered; first match wins. Unit-tested against fixture files (`tests/classifier_fixtures/*.stderr`).

### Event shape

New variant in `EventKind`:

```rust
WorkerError {
    worker_id: Uuid,
    workspace_id: Option<Uuid>,
    issue_id: Option<Uuid>,
    category: WorkerErrorCategory,       // stored as string
    transient: bool,
    retry_after_seconds: Option<u64>,
    human_message: String,
    severity: ErrorSeverity,
    raw_tail: String,                    // last ~2 KiB of stderr, for forensics
}
```

Persisted to `events` table. pg_notify broadcasts to `/ws/events` subscribers (automatic via existing machinery). Indexed on `repo_id`, `workspace_id`, `worker_id` — supports both workspace-panel queries and account-health aggregation.

### Account health derivation

Not stored. Computed on query:

```sql
SELECT category, COUNT(*) AS affected, MIN(created_at) AS since
FROM events
WHERE user_id = $1                       -- resolved from API key
  AND event_kind = 'WorkerError'
  AND jsonb_extract_path_text(payload, 'category') IN (
    'AnthropicRateLimit', 'AnthropicAuthFailed',
    'AnthropicQuotaExceeded', 'AnthropicOverloaded'
  )
  AND created_at > now() - interval '5 minutes'
GROUP BY category
ORDER BY MIN(created_at) ASC;
```

Endpoint `GET /api/me/agent-health` returns aggregated state. 30s server-side cache. Dashboard polls every 30s or subscribes via WebSocket for zero-lag updates.

Status mapping:
- **OK** — zero Class A errors in window
- **Degraded** — only transient Class A errors (rate limits self-clearing)
- **Failing** — non-transient Class A (auth / quota / outage >5 min) → requires user action

## Data flow (happy path + error path)

### Happy path

```
User creates issue
  → server spawns worker (PRD 28 path)
  → worker POSTs logs + heartbeats (PRD 28 path)
  → dashboard's workspace page subscribes to WS events filtered by workspace_id
  → renders live status + streaming logs
  → worker submits, workspace marked Submitted, issue closed
  → dashboard panel collapses to retrospective summary
```

### Error path

```
claude -p exits 1 with rate_limit stderr
  → loop.sh: `vai agent error --stderr-file ... --phase claude --wait`
  → CLI POSTs raw stderr to /api/agent-workers/:id/error
  → server classifier matches AnthropicRateLimit pattern
  → writes EventKind::WorkerError event (transient=true, retry_after=60)
  → pg_notify broadcasts to dashboard WS subscribers
  → dashboard receives event:
     - plan-status widget status updates: "Anthropic: rate-limited"
     - top account banner appears (or count increments if already visible)
     - workspace page panel shows "blocked: see banner"
  → server returns {transient: true, retry_after_seconds: 60} to CLI
  → CLI sleeps 60s (because --wait), exits 0
  → loop.sh continues iteration — retries claude invocation
  → success → WorkerError events age out of 5-min window → banner auto-removes
```

## Workspace panel component contract

Single React component at roughly `src/components/workspaces/AgentPanel.tsx`. Inputs:

- `workspaceId: string`
- `workspace: Workspace` (from TanStack Query cache)

Data hooks:
- `useWorkersForWorkspace(workspaceId)` — `GET /api/agent-workers?workspace_id=`
- `useWorkerLogs(workerId)` — paged `GET /api/agent-workers/:id/logs`; live-updates via WS events
- `useWorkerEvents(workspaceId)` — WS-derived store of `WorkerError` + state change events for this workspace

Render branches:

| Workspace state | Has worker? | Render |
|---|---|---|
| Created / Active | yes, running | Live: phase banner + heartbeat + tabbed (stream / full / timeline) |
| Created / Active | yes, dead/failed | Failure-first: error prominence + timeline + full log + retry CTA |
| Submitted | yes | Collapsed summary, expandable |
| Discarded | yes | Collapsed summary with "discarded" framing |
| any | no | Hidden entirely |

Sub-tabs inside the panel when live:
- **Stream** — autoscroll log view; bottom-sticks unless user scrolls up
- **Full** — searchable full log (stdout + stderr), filter by stream, copy button
- **Timeline** — state transitions with timestamps and delta

Filters / actions:
- Stream tag (stdout vs stderr) distinguished by color in stream view
- Relative timestamps ("+12s since claim"); absolute on hover
- "Kill worker" action only when state is `running` and user has admin role

## Account banner component

Mounted at dashboard root layout, above the main content. Subscribes to `GET /api/me/agent-health` (polled 30s OR WS events via filtered event stream).

States:
- Hidden when status = OK
- Yellow banner when Degraded: "Anthropic: recovering from rate limits (transient, retrying automatically)"
- Red banner when Failing with action CTA: "Anthropic: auth failed — rotate key in Settings" / "Anthropic: monthly quota exceeded — upgrade plan" / etc.

One banner even when N workers across M repos are affected. Count ("12 workers affected") shown in the banner body.

Dismissible? No — banner persists while condition is live. Self-removes when condition clears. (User can always collapse to a single-pixel status strip if they want.)

## Files to build on

### New

- `src/server/worker_error_classifier.rs` — server-side classifier + `WorkerErrorCategory` enum
- `src/cli/agent_cmd.rs` — new `error` subcommand; wires stdin/file → POST
- `docker/worker/loop.sh` — wrap `claude -p` invocation; on non-zero exit, pipe stderr to `vai agent error --wait`
- Dashboard: `src/components/workspaces/AgentPanel.tsx` — state-aware adaptive panel
- Dashboard: `src/components/agents/AccountHealthBanner.tsx` — top-level banner
- Dashboard: `src/hooks/use-workers-for-workspace.ts`, `use-worker-logs.ts`, `use-agent-health.ts`

### Extend

- `src/event_log/mod.rs` — add `EventKind::WorkerError` variant
- `src/server/issue.rs` / `src/server/worker.rs` — add `POST /api/agent-workers/:id/error` endpoint
- `src/server/worker.rs` — add list endpoint `GET /api/agent-workers?workspace_id=...&repo_id=...`
- `src/server/me.rs` — add `GET /api/me/agent-health` endpoint (reads events, 30s cache)
- `src/server/ws.rs` — no change needed; `EventKind::WorkerError` is broadcast by existing pg_notify machinery
- Dashboard: existing workspace detail route(s) — embed `AgentPanel`
- Dashboard: existing root layout — mount `AccountHealthBanner`

## Phased delivery

Two phases within this PRD. Part 2 handles the bits deferred during the design session.

### Phase A — error classification plumbing (unblocks everything)

1. `EventKind::WorkerError` variant + migration (if needed for indexed query; existing events table may suffice)
2. Server-side classifier module + unit tests against fixture files
3. `POST /api/agent-workers/:id/error` endpoint
4. `vai agent error` CLI subcommand + integration tests
5. `loop.sh` update: wrap claude, use `vai agent error --wait` on failure

No UI yet. Dashboard unaffected. This phase makes cloud workers behave correctly under rate-limit / auth-failure conditions without crashing or spinning.

### Phase B — workspace panel + account banner

1. `GET /api/agent-workers?workspace_id=&repo_id=` list endpoint
2. `GET /api/me/agent-health` endpoint
3. `AgentPanel` component + all three render branches
4. `AccountHealthBanner` component
5. WebSocket subscription wiring in existing `use-events` hook
6. E2E test: create an issue with a seeded "will fail" hint, watch agent panel surface the failure; trigger a fake rate-limit event, watch banner

### Out of scope here — follow-on PRD 30 or similar

- **Real-time delivery details**: WS event batching for high-volume log streams (500+ chunks/min observed in smoke); fallback polling when WS disconnects
- **Log volume handling**: server-side chunk batching; client-side virtualization; drop-old-chunks at X MB per worker
- **Security / privacy**: who can see logs (RepoRole::Read gate vs Admin); env-var scrubbing in stderr before persistence
- **Retention**: respecting `plans.log_retention_days` in queries and in UI ("logs aged out, summary only")
- **Worker kill / retry actions**: UI wiring + auth for `DELETE /api/agent-workers/:id` and a new retry endpoint
- **Multi-key accounts**: if a user has multiple repos with different Anthropic keys, health partitions per-key
- **Push notifications / email alerts**: when account health goes Failing, user gets a message even if dashboard isn't open

## Open risks

- **Classifier drift** — Claude CLI error format changes without warning. Mitigation: `Uncategorized` bucket captures novel patterns; monitor for spikes; fixture-test workflow before each release.
- **Event volume scaling** — at 5 workers × 500 chunks/min, WS traffic could overwhelm slow clients. Chunks-per-second throttle + batch windows should handle this; exact design in Part 2.
- **Privacy of stderr** — worker stderr may contain env values (API keys leaked by misbehaving processes). Classifier should strip well-known key shapes before persistence; investigate during implementation.
- **Account health false positives** — one flaky worker shouldn't tip the account banner to red. Aggregation window (5 min) + minimum count (≥3 workers affected for Degraded) should mute noise.
- **Retrospective view of aged-out logs** — free-tier users have 1 day retention; a workspace panel viewed later has nothing to show. Needs UX: "logs were retained until T; upgrade for longer retention."

## Verification

1. Classifier unit tests with fixture stderr files (rate limit, auth failure, quota, overloaded, OOM, CLI crash, unknown)
2. Integration test: worker hits Anthropic 429, `vai agent error --wait` sleeps correctly, retry succeeds
3. E2E test: simulated rate-limit event → workspace panel displays it; AccountHealthBanner appears; 5-min window expires → banner self-removes
4. Manual: cut a release with Phase A, enable cloud on vai-dashboard-test, force an Anthropic 401 (rotate key mid-run), confirm banner and workspace panel both reflect it
