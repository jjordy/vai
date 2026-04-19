# PRD 26: User Onboarding — Test Plan

Systematic end-to-end verification of the full onboarding flow: install CLI → login → init repo → generate loop → create first issue → run loop.

This plan maps every acceptance criterion from PRD 26's 21 issues (V-1..V-14, D-1..D-7) to a concrete verification step. Execute top-to-bottom, mark each box as pass (`[x]`), fail (`[✗]`), or blocked (`[·]`) with a one-line note.

## Setup

Before starting, confirm these preconditions. If any fail, stop and fix before proceeding — the journeys below assume all are green.

- [ ] `vai-server-polished-feather-2668.fly.dev` health endpoint returns 200 with `database.healthy: true` (`curl https://vai-server-polished-feather-2668.fly.dev/api/health`).
- [ ] Dashboard dev server running on `http://localhost:3000` (`ps aux | grep vite`).
- [ ] Fresh Linux or macOS VM/container available (LXC, Multipass, or Docker container). Journey 1 must start from **truly nothing installed**.
- [ ] Latest `vai` release tag exists on GitHub with all 4 target binaries + `SHA256SUMS` (V-1).
- [ ] You have clipboard access + a browser on the host that's running this test.
- [ ] Test account email reserved (e.g. `test-onboarding+$(date +%s)@example.com`).

### Tools

- **Terminal** — for CLI commands. Keep two open (one for CLI loop, one for inspecting state).
- **Browser devtools → Network tab** — for polling verification.
- **`curl` / `jq`** — for direct API inspection.
- **Playwright MCP** (optional) — for scripted UI walkthroughs.

---

## Journey 1: Brand-new user (the happy path)

Fresh machine, no vai account, no CLI, no credentials. This is the flow PRD 26 is designed for.

### Stage 1 — Sign up & land on /welcome

- [ ] Open browser to dashboard root, click "Sign up". Better Auth signup works. Land on `/welcome` automatically. *(D-1)*
- [ ] `/welcome` shows the 5-step checklist with correct visual states (1 current `⊙`, 2-5 pending `○`). *(D-1)*
- [ ] Each step has a copy-to-clipboard button on its command. Click step 1's button, paste into terminal — exact command appears. *(D-1)*
- [ ] "Skip" link visible bottom-right. *(D-1)*
- [ ] Open devtools → Network. Confirm polling every ~3 seconds on: `GET /api/keys`, `GET /api/repos`. *(D-1)*
- [ ] `user.onboarding_completed_at` is `NULL` — confirm via SQL: `psql ... -c "SELECT onboarding_completed_at FROM \"user\" WHERE email='<test-email>'"`. *(D-2)*

### Stage 2 — Install CLI (V-1)

Run on the fresh VM.

- [ ] `curl -fsSL https://vai.dev/install.sh | sh` completes without prompting for sudo if `~/.local/bin` writable + on PATH. *(V-1)*
- [ ] Binary landed at `~/.local/bin/vai` and has exec bit. *(V-1)*
- [ ] Install script refuses to run when invoked as root: `sudo curl -fsSL .../install.sh | sh` prints warning and exits non-zero. *(V-1)*
- [ ] Warning printed if `~/.local/bin` is not on PATH. *(V-1)*
- [ ] `vai --version` prints a non-placeholder version string matching the latest release tag. *(V-1)*
- [ ] SHA256 check: if you manually corrupt the downloaded tarball before extraction, install fails loudly. *(V-1)*

### Stage 3 — `vai login` (browser flow) (V-4, D-3)

Run `vai login` on the VM (needs a browser — use the host's browser if testing in a container with port forwarding).

- [ ] CLI binds an ephemeral port in [49152, 65535]. *(V-4)*
- [ ] Browser opens to `$DASHBOARD_URL/cli-auth?port=NNNN&state=<hex>&hostname=<host>`. *(V-4)*
- [ ] `/cli-auth` renders approval UI: shows hostname + account email + Authorize/Cancel buttons. *(D-3)*
- [ ] `/cli-auth` redirects to login if not authenticated. *(D-3)*
- [ ] Click Authorize. Within ~2 seconds:
  - Terminal unblocks with `Logged in as <email>.` *(V-4)*
  - Dashboard shows "You can close this tab and return to your terminal." *(D-3)*
  - `~/.vai/credentials.toml` exists with mode `0600`. *(V-4)*
  - File contains `[default]` block with `server_url`, `api_key`, `user_id`, `user_email`. *(V-4)*
  - API key name is `CLI on <hostname>` — confirm via dashboard `/settings/keys` page or `curl -H "Authorization: Bearer <key>" .../api/keys`. *(D-3)*
- [ ] Back on `/welcome` (refresh or let it poll): **step 2 flips to complete within 5 seconds** (no page refresh needed). *(D-1)*
- [ ] Invalid port in URL (`/cli-auth?port=80`) → shows validation error, does not render Authorize button. *(D-3)*
- [ ] Missing `state` → validation error. *(D-3)*

### Stage 4 — Device code flow (V-3, V-4, D-3)

Run on the VM with browser disabled: `unset DISPLAY WAYLAND_DISPLAY && vai login`.

- [ ] CLI auto-detects headless, falls back to device mode (or explicitly use `vai login --device`). *(V-4)*
- [ ] Terminal prints `Visit <dashboard>/cli and enter code: XXXX-YYYY`. Code format matches `[A-Z0-9]{4}-[A-Z0-9]{4}`. *(V-3)*
- [ ] CLI polls `GET /api/auth/cli-device/:code` every 3 seconds — confirm via `tcpdump` or by reading the server logs. *(V-4)*
- [ ] `/cli` page loads with a code input box. *(D-3)*
- [ ] Enter the code + submit. Dashboard shows "CLI authorized." Terminal unblocks within one poll interval. *(D-3, V-3)*
- [ ] Repeated `GET /api/auth/cli-device/:code` after authorization returns 404 (single-use). *(V-3)*
- [ ] Wait 10 minutes past issuance → `GET` returns `{ status: "expired" }`. *(V-3)*
- [ ] `/cli` with invalid code → "Code not recognised." *(D-3)*
- [ ] Running `vai login` again cleanly overwrites `~/.vai/credentials.toml`. *(V-4)*

### Stage 5 — `vai init` (V-5, V-6, V-2)

In a fresh project directory on the VM (e.g. `mkdir ~/test-app && cd ~/test-app && npm init -y`).

- [ ] `vai init` prints `Initializing vai repo in <path>...` followed by `✓ Created .vai/`, `✓ Wrote vai.toml and .vai/config.toml`, `✓ Registered repo "<basename>"`, `✓ Pushed initial snapshot (N files, N.N MB)`. *(V-5)*
- [ ] `.vai/config.toml` contains `repo_id` + `[remote]` block with `url` and `repo_name`. *(V-5)*
- [ ] **`.vai/config.toml` contains NO `api_key` field anywhere.** Confirm with `grep -i api_key .vai/config.toml` — empty output. *(V-6)*
- [ ] Dashboard `/$repoSlug/` shows the repo exists and has an initial version. *(V-2)*
- [ ] User is admin collaborator on the repo. Confirm via SQL: `SELECT role FROM repo_collaborators WHERE user_id = '<id>' AND repo_id = '<id>'`. Expected: `admin`. *(V-2)*
- [ ] `/welcome` step 3 flips to complete within 5 seconds. *(D-1)*
- [ ] `.env` is added to `.gitignore` if absent — confirm line `.env` present in `.gitignore` after init. *(V-5)*

**Collision handling**:
- [ ] `cd ~ && mkdir test-app-2 && cd test-app-2 && vai init`. Since `test-app-2` is a new name, should register cleanly.
- [ ] Now `cd ~ && mkdir duplicate-test && cd duplicate-test` — rename dir so basename collides with existing repo. Run `vai init`. CLI prompts `Repo name 'test-app' already taken. Try a different name? [test-app-2]:`. *(V-5)*
- [ ] Accept the suggested name → registration succeeds with the alternative name. *(V-5)*

**Flags**:
- [ ] `vai init --local-only` in a fresh dir creates `.vai/` but makes no network call (confirm with `strace -f -e trace=network` or by disconnecting network). *(V-5)*
- [ ] `vai init --no-push` registers but doesn't push — server `GET /api/repos/<name>/versions` returns empty. *(V-5)*
- [ ] `vai init --remote-name foo` registers the repo as `foo` regardless of directory name. *(V-5)*

**Quota & payload warnings** (these need specific test conditions):
- [ ] Create 100 repos, then try a 101st → 403 with body `{"error":"repo quota exceeded","limit":100,"current":100}`. Print the full error to the user. *(V-2)*
- [ ] In a directory >100 MB tracked, `vai init` prints top 5 largest paths and prompts `Push anyway? [y/N]`. On `N`, exits 0 with `.vai/` created but no push. *(V-5)*

### Stage 6 — `vai agent loop init` (V-7, V-8, V-9, V-10, V-11)

In `~/test-app` (still a fresh React-ish project: `npm init -y && npm install react`).

- [ ] `vai agent loop --help` shows `init`, `run`, `list` subcommands. *(V-7)*
- [ ] `vai agent loop init` prints `Detected project type: frontend-react (found package.json with "react")`. *(V-8)*
- [ ] Accepts interactive input for agent choice (claude-code / codex / custom). Default = claude-code. *(V-11)*
- [ ] Accepts interactive input for mode (Docker / bare). Default = Docker if `docker` on PATH. *(V-11)*
- [ ] After completion, these files exist and are non-empty:
  - `.vai/agent.toml` with `[checks]` block matching frontend-react template *(V-9)*
  - `.vai/agents/claude-code/prompt.md` (has three-phase workflow + Playwright MCP refs for frontend-react) *(V-9)*
  - `.vai/agents/claude-code/loop.sh` (executable, mode 0755) *(V-11)*
  - `.vai/agents/claude-code/Dockerfile` (since Docker mode) *(V-11)*
- [ ] `.env` at repo root contains a `# --- vai loop (added YYYY-MM-DD) ---` block with:
  - `VAI_API_KEY=vk_live_...` (actual key, server-minted) *(V-10)*
  - `# Claude Code OAuth token — run \`claude setup-token\` and paste the token here:` comment *(V-10)*
  - `CLAUDE_CODE_OAUTH_TOKEN=` (empty placeholder) *(V-10)*
- [ ] `POST /api/keys` was called server-side with `name="loop-test-app"`, `role="write"`, `repo_scope=<repo_id>`. Confirm via dashboard `/settings/keys`. *(V-10)*
- [ ] Generated `loop.sh` passes `shellcheck`. *(V-11)*
- [ ] Running `vai agent loop init` a second time does NOT duplicate the `.env` block (idempotent). *(V-10)*
- [ ] Generated Dockerfile builds successfully: `cd .vai/agents/claude-code && docker build -t test-loop .`. *(V-11)*

**Project type overrides**:
- [ ] Fresh dir with only `Cargo.toml` → `vai agent loop init` detects `backend-rust`. *(V-8)*
- [ ] Fresh dir with only `package.json` (no React) → detects `backend-typescript`. *(V-8)*
- [ ] Empty dir → detects `generic`. *(V-8)*
- [ ] Full-stack dir (both `Cargo.toml` + React in `package.json`) → detects `frontend-react`. *(V-8)*
- [ ] `vai agent loop init --project-type backend-rust` overrides detection. *(V-8)*
- [ ] `vai agent loop init --name experimental` creates `.vai/agents/experimental/` instead of `.vai/agents/claude-code/`. *(V-11)*

### Stage 7 — Welcome page auto-completion (D-1, D-2)

- [ ] `/welcome` step 4 flips to complete once any repo has an issue — i.e., proxies for "loop generated". *(D-1, per PRD: step 4 auto-completes when step 5 becomes available)*
- [ ] Click step 5's CTA button → navigates to `/<repoSlug>/issues/new`. *(D-1)*
- [ ] Create an issue. Back on `/welcome`: step 5 flips complete within 5 seconds. *(D-1)*
- [ ] Once all 5 steps complete, page auto-POSTs to `/api/me/skip-onboarding` — confirm via devtools → Network. *(D-2)*
- [ ] `user.onboarding_completed_at` is now set (not NULL). *(D-2)*
- [ ] Refresh `/welcome` → redirects to `/`. *(D-2)*
- [ ] Explicitly visit `/welcome?force=1` → page still renders (bypass works). *(D-2)*

### Stage 8 — `vai agent loop run` (V-13)

In `~/test-app`. Assumes `.env` has been filled in (either you set `CLAUDE_CODE_OAUTH_TOKEN` or accept that it'll fail pre-flight).

- [ ] Without `CLAUDE_CODE_OAUTH_TOKEN` set: `vai agent loop run` fails with `Error: CLAUDE_CODE_OAUTH_TOKEN is empty.` and prints the specific line number in `.env` to edit. *(V-13)*
- [ ] With Docker mode but Docker daemon stopped: fails with `Error: Docker daemon not running.` *(V-13)*
- [ ] With everything set: `exec`s the loop script (`ps` shows `loop.sh` replacing the `vai` process). *(V-13)*
- [ ] With multiple configs in `.vai/agents/*` and no default in `agent.toml`: prompts to pick. *(V-13)*
- [ ] `--name <name>` picks the right one. *(V-13)*
- [ ] `.vai/agents/<name>/.last-run` file is touched before exec. *(V-14)*

### Stage 9 — `vai agent loop list` (V-14)

- [ ] Zero configs: `vai agent loop list` prints `No loops configured. Run 'vai agent loop init' to create one.` *(V-14)*
- [ ] With the claude-code config from stage 6: table shows `claude-code | docker | last run: <time>`. *(V-14)*
- [ ] Add a second config with `vai agent loop init --name codex --agent codex --no-docker`. List now shows both rows. *(V-14)*

### Stage 10 — `vai agent prompt` overlay (V-12)

In `~/test-app` with an active loop config.

- [ ] With no `.vai/custom-prompt.md`: `vai agent prompt` outputs `<base template> + <blank> + <issue JSON>`. *(V-12)*
- [ ] Create `.vai/custom-prompt.md` with `# Custom project guidance\nAlways prefer functional style.`. Now `vai agent prompt` outputs `<base> + <custom> + <issue>`. *(V-12)*
- [ ] Legacy path: delete `.vai/agents/claude-code/prompt.md` and create `.vai/prompt.md` instead. `vai agent prompt` still works (fallback). *(V-12)*
- [ ] Behavior is identical for users who never create `custom-prompt.md` (no warnings, no crashes). *(V-12)*

---

## Journey 2: Returning user on a new machine

User already has an account + some repos. Tests key reuse, skip-onboarding, existing state paths.

- [ ] From a different fresh VM: `curl install.sh | sh && vai login`. New API key minted, named `CLI on <new-hostname>`. Old keys still visible in `/settings/keys`. *(V-4, D-3)*
- [ ] Visit `/welcome?force=1`. Step 2 already complete (previous key matches `CLI on *`). Step 3 already complete (has a repo). Step 5 already complete (has an issue). Only steps 1 + 4 pending/current. *(D-1)*
- [ ] `user.onboarding_completed_at` was set on first account → `/welcome` without `?force=1` redirects to `/`. *(D-2)*
- [ ] Sidebar shows "Getting Started" + "Help" entries always visible. Click Getting Started → navigates to `/welcome?force=1`. *(D-6)*
- [ ] Click Help → navigates to `/help` → shows platform docs README. *(D-4, D-6)*
- [ ] On `vai init` in a new local dir with same basename as existing repo → rename prompt triggers. *(V-5)*

---

## Journey 3: Power user — custom prompts + multiple loops

Tests the escape hatches for users who want more control.

- [ ] In an existing repo, create `.vai/custom-prompt.md` with project-specific guidance. `vai agent prompt` now includes it. *(V-12)*
- [ ] Commit `.vai/custom-prompt.md` to source control (it should be trackable — confirm no `.gitignore` rule excludes it). *(V-12)*
- [ ] Run `vai agent loop init --name planner --agent custom` in the same repo. Creates `.vai/agents/planner/` alongside existing `.vai/agents/claude-code/`. *(V-11)*
- [ ] `vai agent loop list` shows both configs. *(V-14)*
- [ ] `vai agent loop run --name planner` execs the planner loop; `vai agent loop run` (no flag) prompts to pick. *(V-13)*
- [ ] Two separate `VAI_API_KEY` env files are NOT required — both configs share the same `.env` at repo root. *(V-10)*
- [ ] Re-run `vai agent loop init --overwrite --name claude-code`. Existing `prompt.md` is backed up to `prompt.md.bak.YYYYMMDD-HHMMSS`. *(V-11)*

---

## Platform docs verification (D-4, D-5)

Can be done any time during or after the journeys.

- [ ] `/help` loads and renders `docs/platform/README.md`. *(D-4)*
- [ ] Left sidebar shows file tree of `docs/platform/`. *(D-4)*
- [ ] All 6 content files exist and meet minimum length requirements:
  - `README.md` ≥30 lines *(D-4)*
  - `getting-started.md` ≥100 lines *(D-4)*
  - `planning-pattern.md` ≥50 lines *(D-4)*
  - `writing-good-issues.md` ≥40 lines *(D-4)*
  - `customising-prompts.md` ≥40 lines *(D-4)*
  - `running-multiple-loops.md` ≥30 lines *(D-4)*
- [ ] `/help/planning-pattern` renders with `{{VAI_URL}}` substituted with the live server URL and `{{REPO}}` substituted with the user's default repo slug. *(D-5)*
- [ ] `/help/nonexistent` returns a 404 message with a link back to `/help`. *(D-4)*
- [ ] `<MarkdownRenderer content="foo {{X}} bar" tokens={{X:"baz"}} />` → renders `foo baz bar`. Test with a direct component render or a unit test. *(D-5)*
- [ ] Missing tokens render the literal `{{X}}` unchanged. *(D-5)*

---

## Empty state (D-7)

- [ ] As a brand-new user with zero repos, visit `/`. Repo picker empty state shows "No repos yet" + CTA button "Get Started →" linking to `/welcome?force=1`. *(D-7)*
- [ ] User with ≥1 repo sees the normal repo list (no change to that path). *(D-7)*

---

## Reconciliation

After executing the plan, tally results into one of:

- **All green** → ship it. Post test-plan.md + results to the PRD 26 issue and close.
- **Flakes / transient failures** → re-run flaky steps; if reproducible, file a bug issue.
- **Real bugs** → file per-item GitHub issues on the right repo (vai or vai-dashboard) with the exact failing step pasted and acceptance criteria linked. Gate the PRD 26 "done" state on those fixes.

## Coverage map

Issues verified by this plan:

| Issue | Covered in stage |
|--|--|
| V-1 | Setup + Stage 2 |
| V-2 | Stage 5 (quota, admin auto-assignment) |
| V-3 | Stage 4 |
| V-4 | Stages 3, 4 |
| V-5 | Stage 5 |
| V-6 | Stage 5 (config.toml inspection) |
| V-7 | Stage 6 |
| V-8 | Stage 6 |
| V-9 | Stage 6 |
| V-10 | Stage 6 |
| V-11 | Stage 6, 8 |
| V-12 | Stage 10 |
| V-13 | Stage 8 |
| V-14 | Stage 9 |
| D-1 | Stages 1, 7 |
| D-2 | Stages 1, 7 |
| D-3 | Stages 3, 4 |
| D-4 | Platform docs section |
| D-5 | Platform docs section |
| D-6 | Journey 2 |
| D-7 | Empty state section |

Every acceptance criterion in every issue's description should trace to one (or more) checkboxes above. If you find an acceptance criterion with no verification step, add one inline — the plan grows organically as gaps surface.
