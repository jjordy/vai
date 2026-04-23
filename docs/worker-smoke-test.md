# Worker Smoke Test Runbook

Runbook for Phase 2 of PRD 28: verifying the canonical worker image end-to-end on
staging before enabling cloud mode for real repos.

## Prerequisites

- Staging vai server running with Postgres + the Phase 2 migrations applied
- `VAI_COMPUTE_FLY_TOKEN` secret set in the staging Fly app
- `VAI_ADMIN_KEY` available locally for admin API calls
- A published `ghcr.io/jjordy/vai-worker` image (see next section)
- `fly` CLI installed and authenticated

---

## 1. Cutting a pre-release image

The `publish-worker-image.yml` workflow fires on any `v*` tag. To publish without a
full release, push a pre-release tag:

```bash
git tag v0.1.8-rc.1
git push origin v0.1.8-rc.1
```

GitHub Actions builds `linux/amd64` + `linux/arm64` and pushes:
- `ghcr.io/jjordy/vai-worker:0.1.8-rc.1`
- `ghcr.io/jjordy/vai-worker:latest`

After the first publish the GHCR package is private. Make it public before testing:
**github.com/jjordy → Packages → vai-worker → Package settings → Change visibility → Public**

Verify the image boots:
```bash
docker run --rm ghcr.io/jjordy/vai-worker:0.1.8-rc.1 vai --version
```

---

## 2. Enabling cloud mode on a staging repo

Cloud mode is controlled by `repos.cloud_agent_enabled` in Postgres. There is no
UI toggle yet (Phase 3); flip it directly via SQL or the admin API.

### Via SQL (psql)

```sql
UPDATE repos SET cloud_agent_enabled = TRUE WHERE name = '<repo-name>';
```

### Via admin API

```bash
# Replace values as appropriate
VAI_ADMIN_KEY=<your-admin-key>
REPO_ID=<uuid>
SERVER=https://vai-staging.example.com

curl -s -X PATCH "$SERVER/api/repos/$REPO_ID" \
  -H "Authorization: Bearer $VAI_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"cloud_agent_enabled": true}'
```

Confirm the flag is set:
```bash
curl -s "$SERVER/api/repos/$REPO_ID" \
  -H "Authorization: Bearer $VAI_ADMIN_KEY" | jq .cloud_agent_enabled
```

---

## 3. Running the bare E2E smoke test

### Step 1 — Create a test issue

```bash
curl -s -X POST "$SERVER/api/issues" \
  -H "Authorization: Bearer $VAI_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"repo_id":"<REPO_ID>","title":"smoke test","body":"echo hello and submit","priority":"high"}' \
  | jq .id
```

The issue-creation handler calls `spawn_if_capacity` (PRD 28 §Architecture data flow).
If capacity is available, `FlyMachinesProvider::spawn()` fires and creates a machine.

### Step 2 — Confirm the machine booted

```bash
fly machine list --app vai-workers
```

You should see a machine in `started` state tagged with the worker UUID.

### Step 3 — Watch vai-server logs for the worker

```bash
# Worker heartbeat — server logs will show:
fly logs --app vai-server | grep "heartbeat"

# Or query the worker directly once you have its UUID:
WORKER_ID=<uuid-from-issue-or-fly-tags>

# Heartbeat state:
curl -s "$SERVER/api/agent-workers/$WORKER_ID" \
  -H "Authorization: Bearer $VAI_ADMIN_KEY" | jq '{state,last_heartbeat_at}'
```

### Step 4 — Stream logs

```bash
curl -s "$SERVER/api/agent-workers/$WORKER_ID/logs" \
  -H "Authorization: Bearer $VAI_ADMIN_KEY" | jq .
```

Or fallback to Fly's native log stream:
```bash
fly logs --app vai-workers
```

### Step 5 — Verify the cycle completed

After the worker finishes, the agent_workers row should show `state: completed`:

```bash
curl -s "$SERVER/api/agent-workers/$WORKER_ID" \
  -H "Authorization: Bearer $VAI_ADMIN_KEY" | jq .state
```

The issue should be closed or have a submitted workspace attached. The dead-worker
reconciliation cron (~60s) will clean up any orphaned state.

---

## 4. Canary tests (inside the canonical image)

These prove the image ships working toolchains for the two primary target repos.
Run both against the same image tag used in staging.

### vai-dashboard canary

```bash
# From the vai-dashboard repo root:
docker run --rm \
  -v "$(pwd):/w" -w /w \
  ghcr.io/jjordy/vai-worker:<tag> \
  bash -c 'pnpm install --frozen-lockfile && pnpm typecheck && pnpm test && playwright test'
```

### vai canary

```bash
# From the vai repo root:
docker run --rm \
  -v "$(pwd):/w" -w /w \
  ghcr.io/jjordy/vai-worker:<tag> \
  bash -c 'cargo clippy --features full -- -D warnings && cargo test --features full'
```

Both must pass unmodified. If a toolchain version in the image does not match what
a repo needs, either bump the image or add a `.tool-versions` / `mise.toml` file to
the repo — `mise` runs automatically in `loop.sh`.

---

## 5. Rolling back

### Disable cloud mode

```bash
# SQL:
UPDATE repos SET cloud_agent_enabled = FALSE WHERE name = '<repo-name>';

# Or via admin API:
curl -s -X PATCH "$SERVER/api/repos/$REPO_ID" \
  -H "Authorization: Bearer $VAI_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"cloud_agent_enabled": false}'
```

New issues will no longer trigger worker spawning. In-flight workers continue to
their natural end.

### Kill stuck machines

If a machine is stuck (e.g. heartbeat stopped, worker never called `/done`):

```bash
# List machines for the vai-workers app:
fly machine list --app vai-workers

# Destroy a specific machine:
fly machine destroy <machine-id> --app vai-workers --force

# Or via the admin API once the destroy endpoint is implemented (Phase 2+):
curl -s -X DELETE "$SERVER/api/agent-workers/$WORKER_ID" \
  -H "Authorization: Bearer $VAI_ADMIN_KEY"
```

The dead-worker reconciliation cron marks the corresponding `agent_workers` row
`dead` within ~60 seconds after the machine disappears from the Fly API.
To trigger it immediately, restart vai-server: the cron fires on startup.

### Requeue a stuck issue

If a worker died holding a claimed workspace, the reconciliation cron reopens the
issue automatically. To force it manually:

```bash
# Find the workspace that was claimed by the dead worker:
psql $DATABASE_URL -c \
  "SELECT w.id FROM workspaces w
   JOIN agent_workers aw ON aw.workspace_id = w.id
   WHERE aw.id = '<WORKER_ID>'"

# Discard the workspace — this reopens the linked issue:
vai workspace discard <workspace-id>   # run from a machine with vai CLI + DB access
```

---

## Known failure modes from smoke runs

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| Machine boots but immediately exits | `VAI_API_KEY` or `ANTHROPIC_API_KEY` not injected by orchestrator | Check `repo_agent_secrets` row exists; verify `FlyMachinesProvider::spawn()` injects env vars |
| Worker claims but `vai agent download` fails | Staging server URL wrong or S3 credentials missing | Check `VAI_SERVER_URL` env var in machine; verify S3 bucket policy |
| `cargo test` fails in vai canary | Rust toolchain version mismatch | Check `rust-toolchain.toml` in vai repo vs `rustup default stable` in image; pin image to same channel |
| `playwright test` fails in dashboard canary | Chromium not found | Verify `PLAYWRIGHT_BROWSERS_PATH=/usr/local/share/playwright-browsers` is set; re-run `playwright install chromium` in image build |
| Worker logs never appear in vai-server | Log shipping backpressure or wrong worker UUID | Check `VAI_WORKER_ID` matches the `agent_workers.id` row; verify `/api/agent-workers/:id/logs` returns 204 not 404 |
| Dead-worker cron does not requeue | Cron not running (SQLite mode vs Postgres) | Confirm server started with `--features full` and Postgres DSN is set |

---

## Acceptance checklist

- [ ] Issue created → worker machine spawned (visible in `fly machine list`)
- [ ] Worker runs claim/download/prompt/submit cycle
- [ ] `GET /api/agent-workers/:id` shows `state: completed` after worker exits
- [ ] Logs arrive at `GET /api/agent-workers/:id/logs` during the run
- [ ] vai-dashboard canary passes inside the image
- [ ] vai canary passes inside the image
- [ ] Links to resulting vai version / issue state / Fly machine logs recorded here
