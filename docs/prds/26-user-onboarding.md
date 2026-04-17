# PRD 26: User Onboarding — From Signup to Running RALPH

## Status

Proposed

## Overview

Today there is no path for a new user to go from "never heard of vai" to "running a RALPH loop against a real repo." The dashboard signup works and `vai agent` subcommands exist, but the dozen steps in between — installing the CLI, authenticating it, registering a repo on the server, generating a loop — are either admin-only, undocumented, or absent. This PRD designs and ships that end-to-end flow so a solo developer can onboard in ~5 minutes with no outside help.

Scope is **solo users only**. The server's org + RBAC machinery (PRD 19) stays in place but is hidden from the dashboard. Team flows are a follow-up PRD.

Work spans two repos:
- **git-for-ai** (this repo) — CLI changes, server endpoint changes, installer, release workflow.
- **vai-dashboard** — welcome flow, platform docs at `/help`, onboarding polling, sidebar entries.

## User Journey

After this PRD lands, the happy path looks like:

1. User discovers vai (marketing site, word of mouth) and clicks "Sign up".
2. Creates account via Better Auth (email+password or GitHub OAuth) — **existing behaviour**.
3. Lands on `/welcome` with a live 5-step checklist.
4. Step 1 — "Install the CLI": copies `curl -fsSL https://vai.dev/install.sh | sh`, runs it in terminal. Installer detects OS/arch, drops the `vai` binary into `~/.local/bin` (or `/usr/local/bin`).
5. Step 2 — "Log in from the CLI": runs `vai login`. Browser opens to `/cli-auth?port=NNNN`. User clicks "Authorize". Dashboard POSTs a fresh API key named `CLI on <hostname>` back to `http://127.0.0.1:NNNN/callback`. CLI writes it to `~/.vai/credentials.toml` (mode 0600). Terminal unblocks. Welcome page polls and checks off step 2.
6. Step 3 — "Initialize a repo": runs `vai init` in an existing project directory. CLI creates `.vai/`, detects no collision, calls `POST /api/repos` with inferred name (directory basename), writes the remote block in `.vai/config.toml`, and auto-pushes the initial snapshot. Welcome page checks off step 3.
7. Step 4 — "Generate an agent loop": runs `vai agent loop init`. CLI detects project type (e.g. `frontend-react`), asks the user to pick an agent (claude-code / codex / custom) and execution mode (Docker / bare), writes `.vai/agents/<agent>/{prompt.md,loop.sh,Dockerfile?}` + `.vai/agent.toml` + `.env` entries (auto-created `VAI_API_KEY`, empty placeholder for provider token with per-agent instructions). Welcome page checks off step 4.
8. Step 5 — "Create your first issue": user clicks the dashboard link on the welcome page → `/$repoSlug/issues/new`. Creates an issue. Welcome page checks off step 5.
9. **(Optional)** Step 6 — "Learn the planning pattern": link to `/help/planning-pattern` in the dashboard platform docs.
10. User runs `vai agent loop run`. Pre-flight checks pass. Loop starts. Issue gets claimed, agent runs, submits.

## Design

### Installation

A single shell command:

```bash
curl -fsSL https://vai.dev/install.sh | sh
```

The script:
- Detects OS (macOS / Linux) and arch (x86_64 / arm64).
- Downloads the matching binary from the latest GitHub release.
- Verifies checksum against `SHA256SUMS` (also hosted on GitHub releases).
- Drops binary in `~/.local/bin` if writable and on `$PATH`, otherwise `/usr/local/bin` (with `sudo` prompt), otherwise prints instructions.
- Prints `vai vX.Y.Z installed. Run 'vai login' to authenticate.`

Windows, Homebrew, cargo install, and npm distribution are **out of scope for v1**.

### CLI authentication — `vai login`

New subcommand `vai login` with two modes:

**Browser callback (default):**
1. CLI binds an ephemeral localhost port (e.g. `127.0.0.1:49152`, random above 49152).
2. Opens the user's browser to `$VAI_DASHBOARD_URL/cli-auth?port=49152&state=<random>` (default dashboard URL: the server's configured dashboard host; override with `--dashboard-url`).
3. Dashboard verifies the user's Better Auth session, asks for confirmation, then mints a new API key on the vai server via existing `POST /api/keys` with `name="CLI on <hostname>"` and `role="write"`, scope `repo_scope=null` (account-wide).
4. Dashboard POSTs the key to `http://127.0.0.1:49152/callback?state=<same>` with JSON body `{ "api_key": "vk_live_...", "user_id": "...", "user_email": "..." }`.
5. CLI verifies `state`, writes `~/.vai/credentials.toml`:
   ```toml
   [default]
   server_url = "https://vai.example.com"
   api_key = "vk_live_..."
   user_id = "..."
   user_email = "user@example.com"
   ```
   with mode 0600. Prints `Logged in as user@example.com.` and exits.
6. Shuts down the localhost listener.

**Device code fallback (`vai login --device`):**
1. CLI calls `POST /api/auth/cli-device` on the vai server. Receives `{ code: "ABCD-1234", verification_url: "https://vai.example.com/cli", poll_interval: 3 }`.
2. CLI prints: `Visit https://vai.example.com/cli and enter code: ABCD-1234`.
3. CLI polls `GET /api/auth/cli-device/:code` every 3 seconds. Response is either `pending`, `authorized (with api_key)`, or `expired`.
4. On authorized: writes credentials.toml, prints success.
5. Auto-detects when to use device mode: no `DISPLAY` env var on Linux, or `open`/`xdg-open` not on PATH, or `--device` flag explicitly passed.

**Credentials file:** `~/.vai/credentials.toml` is the global CLI credential source. Replaces the old `.vai/config.toml` storage of `api_key`. Any CLI command reads credentials in this order: (1) `VAI_API_KEY` env var, (2) `~/.vai/credentials.toml`, (3) error.

### Repo registration — `vai init`

`vai init` changes from "local only" to "local + server-registered":

```
$ vai init
Initializing vai repo in /home/user/myapp...
✓ Created .vai/
✓ Wrote vai.toml and .vai/config.toml
Connecting to vai.example.com as user@example.com...
✓ Registered repo "myapp" (id: 3f2a...)
✓ Pushed initial snapshot (248 files, 1.3 MB)

Repo ready: https://vai.example.com/myapp

Next: vai agent loop init
```

Behaviour:
1. Creates `.vai/` structure as today (`src/repo.rs:291`).
2. Writes `.vai/config.toml` with `repo_id`, `name`, `created_at`, `vai_version`, and new `[remote]` block with `url` and `repo_name` (inferred from directory basename). **No `api_key` field anywhere in config.toml.**
3. Reads credentials from `~/.vai/credentials.toml`. If none, prints `Not logged in. Run 'vai login' first.` and exits 1.
4. Calls `POST /api/repos { name: "myapp" }` on the server.
   - On success: writes the returned repo_id into `.vai/config.toml`'s remote block.
   - On 409 (name taken): prompts `"Repo name 'myapp' already taken. Try a different name? [myapp-2]:"`. Retries up to 3 times.
   - On 403 (quota exceeded): prints `"You've hit the 100-repo limit for your account. Delete unused repos in the dashboard."` and exits 2.
5. Collects files respecting `.gitignore` (existing `src/ignore_rules.rs` — no changes needed).
6. If payload > 100 MB, prints the top 5 largest paths and prompts `"Push anyway? [y/N]"`.
7. Runs an initial push (equivalent to `vai push --message "initial commit"`).
8. Ensures `.env` is in the project's `.gitignore` — if not, appends it and prints a one-line warning.

Flags:
- `--local-only` — skip steps 3–7. Creates `.vai/` without server registration.
- `--no-push` — skip the initial push.
- `--remote-name <name>` — override the inferred name.

### Loop generation — `vai agent loop <init|run|list>`

New subcommand group on `vai agent`. Separate from `vai agent init` (which continues to manage `.vai/agent.toml`).

**`vai agent loop init`** — interactive by default, with flags for scripted use:

```
$ vai agent loop init
Detected project type: frontend-react (found package.json with "react")
Use this template? [Y/n/pick]: Y

Which agent?
  1) claude-code (recommended)
  2) codex
  3) custom (empty template for other agents)
Choice [1]: 1

How should the loop run?
  › Docker (recommended)
    Runs agent in an isolated container. Lets you run multiple loops in
    parallel on the same repo. Requires Docker Desktop (detected ✓).

    Bare shell
    Runs on your host. Faster to start. Only one loop per repo at a time.

Choice [Docker]: Docker

✓ Wrote .vai/agent.toml
✓ Wrote .vai/agents/claude-code/prompt.md (frontend-react template)
✓ Wrote .vai/agents/claude-code/loop.sh
✓ Wrote .vai/agents/claude-code/Dockerfile
✓ Created VAI_API_KEY in .env (scoped to this repo)
✓ Added CLAUDE_CODE_OAUTH_TOKEN placeholder to .env

Next steps:
  1. Run `claude setup-token` and paste the token into .env.
  2. Review .vai/agents/claude-code/prompt.md.
  3. Run `vai agent loop run` to start the loop.
```

Flags:
- `--agent claude-code|codex|custom` — skip agent picker.
- `--project-type frontend-react|backend-rust|backend-typescript|generic` — override detection.
- `--docker` / `--no-docker` — skip mode picker. `--docker` fails if Docker not detected.
- `--overwrite` — rewrite existing files, backing up `prompt.md` to `prompt.md.bak.YYYYMMDD-HHMMSS`.
- `--name <name>` — override the agent directory name (default: `claude-code`). Useful for multiple configs.

**Project-type detection:**
1. `Cargo.toml` + no `package.json` → `backend-rust`.
2. `package.json` with `react`/`vue`/`svelte`/`next`/`remix`/`nuxt` in deps → `frontend-react`.
3. `package.json` without frontend framework → `backend-typescript`.
4. Both `Cargo.toml` and `package.json` with frontend framework → `frontend-react` (the full-stack case: the frontend template has more ceremony and covers more failure modes).
5. Nothing matches → `generic`.

**Embedded templates (included via `include_str!`):**
- `src/cli/agent_loop/templates/frontend-react/prompt.md` — lift vai-dashboard's `/.sandcastle/prompt.md` verbatim (three-phase workflow, Playwright MCP, screenshot verification).
- `src/cli/agent_loop/templates/backend-rust/prompt.md` — three-phase workflow adapted for Rust (cargo check/clippy/test, no browser verification).
- `src/cli/agent_loop/templates/backend-typescript/prompt.md` — Node/Deno/Bun services; tsc + vitest/jest.
- `src/cli/agent_loop/templates/generic/prompt.md` — minimal "read → edit → test → submit".
- `src/cli/agent_loop/templates/agent.toml.{project-type}` — pre-filled `[checks]` block per project type.
- `src/cli/agent_loop/templates/loop.sh.{bare,docker}.{agent}` — loop scripts (one per agent × mode combination).
- `src/cli/agent_loop/templates/Dockerfile.{agent}` — sandcastle Dockerfiles (claude-code only in v1; codex stubbed).

**`agent.toml` generated content for `frontend-react`:**

```toml
server = "https://vai.example.com"
repo = "myapp"

[checks]
setup = ["pnpm install"]
commands = [
  "pnpm biome check --write src/",
  "pnpm tsc --noEmit",
  "pnpm test",
  "pnpm test:e2e"
]
teardown = ["pkill -f 'vite|pnpm dev' 2>/dev/null || true"]

[ignore]
patterns = ["*.lock"]
```

**`.env` handling:**

If `.env` exists at repo root:
- Parse line by line.
- If `VAI_API_KEY=` already present (non-empty), skip auto-creation.
- If `CLAUDE_CODE_OAUTH_TOKEN=` (or equivalent per agent) already present, skip adding placeholder.
- Append a `# --- vai loop (added YYYY-MM-DD) ---` block with any missing keys.

If `.env` doesn't exist, create it with the full block.

Always call `POST /api/keys` during init to mint `VAI_API_KEY` unless it's already populated. Name the key `loop-<repo_name>` (shared across agents on this repo), role=`write`, `repo_scope=<repo_id>`.

**Custom overlay:**

`vai agent prompt` (existing command) is extended to concatenate `.vai/custom-prompt.md` when present:

```
<base template from .vai/agents/<agent>/prompt.md>

<contents of .vai/custom-prompt.md if file exists, otherwise nothing>

<rendered {{issue}} JSON>
```

Order: base → overlay → issue. `.vai/custom-prompt.md` is shared across all agents configured in the repo. Committed to source control (project-wide guidance).

**`vai agent loop run [--name claude-code]`:**

1. Reads `.vai/agent.toml` to resolve default agent name (or `--name`).
2. Pre-flight checks:
   - `VAI_API_KEY` set in `.env`.
   - Agent-specific token set (e.g. `CLAUDE_CODE_OAUTH_TOKEN` for claude).
   - If Docker mode: `docker` on PATH and daemon running.
3. Execs `.vai/agents/<name>/loop.sh`.

If any pre-flight fails, prints the specific line to edit:
```
Error: CLAUDE_CODE_OAUTH_TOKEN is empty.
Edit line 4 of /home/user/myapp/.env and paste the token from `claude setup-token`.
```

**`vai agent loop list`:**

Lists configured agents from `.vai/agents/*`:
```
$ vai agent loop list
claude-code    docker    last run: 2h ago    (active)
codex          bare      never run
```

### Server changes

**Open `POST /api/repos` to any authenticated user.**

Currently (`src/server/admin.rs:127`) this requires admin role. Change:
- Any authenticated user with a non-revoked API key can create a repo.
- Repo is auto-associated with the user as a collaborator with `admin` role on that repo (PRD 19 org model already supports per-repo roles).
- Per-user quota: default 100 repos. Configurable via `VAI_MAX_REPOS_PER_USER` env var.
- Quota check queries `SELECT COUNT(*) FROM repo_collaborators WHERE user_id = $1 AND role = 'admin'`.
- On quota exceeded, return 403 with body `{"error": "repo quota exceeded", "limit": 100, "current": 100}`.

**New endpoints for `vai login` browser callback:**

`POST /api/auth/cli-authorize` (called by the dashboard's `/cli-auth` page):
- Request: `{ state: "<random>", api_key: "<freshly minted>" }` — actually reuses existing `POST /api/keys`, so this endpoint is just a thin wrapper the dashboard calls *on the user's behalf* with a special `create_for_cli: true` flag.
- Easier path: dashboard `/cli-auth` page directly calls `POST /api/keys` (existing) with name/role/scope set appropriately, then POSTs the response body to the CLI's localhost port. No new server endpoint needed.

**New endpoint for device code flow:**

`POST /api/auth/cli-device` — creates a short-lived (10 min) pending CLI device session:
- Generates a user-friendly code (`ABCD-1234`) and a longer internal code.
- Stores in new `cli_device_codes` table: `(code TEXT PRIMARY KEY, user_id UUID NULL, api_key TEXT NULL, expires_at TIMESTAMP, created_at TIMESTAMP)`.
- Returns `{ code, verification_url, poll_interval: 3 }`.

`GET /api/auth/cli-device/:code` — polled by CLI:
- Returns `{ status: "pending" }` or `{ status: "authorized", api_key: "..." }` or `{ status: "expired" }`.
- If authorized, deletes the row after returning once.

`POST /api/auth/cli-device/authorize` — called by dashboard after user enters code:
- Authenticated (requires Better Auth session).
- Request: `{ code: "ABCD-1234" }`.
- Looks up the code, mints an API key via existing key creation, updates the row with `user_id` and `api_key`.

**Dashboard page: `/cli-auth?port=...&state=...`**

- Validates the port is in [49152, 65535] (ephemeral range) and `state` is non-empty.
- Shows: "The vai CLI on `<hostname>`¹ wants to sign in as `user@example.com`. Authorize?" with Approve / Cancel buttons.
  - ¹ `hostname` is optional — CLI can include it as a query param (`&hostname=mbp-jordy`) so the dashboard shows what it's authorizing.
- On Approve: calls `POST /api/keys` with `name="CLI on <hostname>"` (or `"CLI"` if no hostname), `role="write"`, `repo_scope=null`.
- POSTs result to `http://127.0.0.1:<port>/callback?state=<state>` with the full response body.
- Shows success state: "You can close this tab and return to your terminal."

**Dashboard page: `/cli`**

- Input box for device code.
- On submit: calls `POST /api/auth/cli-device/authorize { code }`.
- Success: "CLI authorized. You can close this tab."

### Dashboard onboarding

**Route: `/welcome`**

Shown on first login (and whenever `user.onboarding_completed_at IS NULL`).

Structure:
```
╔══════════════════════════════════════════════════════════╗
║  Welcome to vai                                          ║
║                                                          ║
║  Get your first RALPH loop running in about 5 minutes.  ║
║                                                          ║
║  ✓ 1. Install the CLI                         (complete) ║
║       $ curl -fsSL https://vai.dev/install.sh | sh [📋]  ║
║                                                          ║
║  ✓ 2. Log in from the CLI                     (complete) ║
║       $ vai login                                    [📋]║
║                                                          ║
║  ○ 3. Initialize a repo                       (current)  ║
║       $ cd ~/my-project                                  ║
║       $ vai init                                     [📋]║
║                                                          ║
║  ○ 4. Generate an agent loop                             ║
║       $ vai agent loop init                          [📋]║
║                                                          ║
║  ○ 5. Create your first issue                            ║
║       [ Open in dashboard → ]                            ║
║                                                          ║
║  Optional: Learn the planning pattern →                  ║
║                                                          ║
║                           [I've done this already, skip] ║
╚══════════════════════════════════════════════════════════╝
```

Each step is a card. Each card has:
- A status dot: `○` pending, `⊙` current (next to complete), `✓` complete.
- A heading.
- The exact command to run, with a copy-to-clipboard button.
- For step 5: a button linking to `/$defaultRepoSlug/issues/new`.

**State source:**

The welcome page polls the vai server directly every 3 seconds via existing endpoints (no new server work):
- `GET /api/keys` — if any key with name starting `"CLI on "` exists → step 2 complete.
- `GET /api/repos` — if ≥1 repo visible to this user → step 3 complete.
- For step 4 (loop generated): we can't detect this from the server. **Accept this limitation** — step 4 auto-completes when step 5 becomes available (i.e., we assume if the user has created an issue, they've also generated a loop, because that's the ordered flow).
  - Alternative considered: add a `loop_generated_at` timestamp to `api_keys` when the key is named `loop-*`. Rejected as over-engineering for v1 — the user can dismiss/skip if needed.
- `GET /api/repos/{repo}/issues?limit=1` per repo — if any issue exists → step 5 complete.

**Persistence:**

Add column to `user` table: `onboarding_completed_at TIMESTAMP NULL DEFAULT NULL`.

- Set to `now()` when all 5 steps first report complete.
- Set to `now()` when user clicks "skip".
- Once set, `/welcome` redirects to `/` unless `?force=1` is passed.

**Sidebar:**

Add two new entries after "Docs":
- "Getting Started" — links to `/welcome?force=1`. Always visible.
- "Help" — links to `/help`. Always visible.

### Platform docs at `/help`

New top-level route in vai-dashboard. Reuses the `MarkdownRenderer` component from the existing `/$repoSlug/docs` viewer (PRD 05 in the dashboard repo).

**Content location:** `vai-dashboard/docs/platform/*.md` — committed to the dashboard repo.

**Route structure:**
- `/help` → renders `docs/platform/README.md` (table of contents).
- `/help/*path` → renders `docs/platform/<path>.md`.
- Left panel: file tree over `docs/platform/` (same component as repo docs viewer).
- Right panel: rendered markdown.

**Content files (initial set):**
- `README.md` — "Welcome to vai — start here" with links to other pages.
- `getting-started.md` — expanded version of the welcome checklist with screenshots and troubleshooting.
- `planning-pattern.md` — "Use a local agent to plan features, push issues via the vai API." Includes curl examples with the user's current VAI_URL + REPO.
- `writing-good-issues.md` — short guide on acceptance criteria, scope.
- `customising-prompts.md` — how to edit `.vai/agents/<agent>/prompt.md` and `.vai/custom-prompt.md`.
- `running-multiple-loops.md` — Docker-mode parallel loops.

**Dynamic token substitution:**

`MarkdownRenderer` gains an optional `tokens?: Record<string, string>` prop. Before render, any `{{KEY}}` in markdown source is replaced with `tokens[KEY]`.

For `/help`, tokens = `{ VAI_URL: serverUrl, REPO: defaultRepoSlug || "<repo>" }`.

Example content:
```markdown
## Create an issue from a planning session

```bash
curl -X POST {{VAI_URL}}/api/repos/{{REPO}}/issues \
  -H "Authorization: Bearer $VAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"title":"...","description":"..."}'
```
```

Renders as a runnable command with the user's real URL + repo.

## Issue Breakdown

Work is split by repo. Dependencies arrows indicate hard blockers — dependent issues cannot begin until dependency is merged.

### git-for-ai (vai) — CLI + server

#### V-1: Cross-platform release workflow + install.sh

**Priority:** high
**Blocks:** V-4 (users can't run `vai login` without a binary on PATH)

Set up GitHub Actions release workflow and shell installer.

**Files:**
- `.github/workflows/release.yml` — triggers on tag push `v*`, builds binaries for:
  - `x86_64-unknown-linux-gnu`
  - `aarch64-unknown-linux-gnu`
  - `x86_64-apple-darwin`
  - `aarch64-apple-darwin`
  - Uses cross-compilation or self-hosted runners. Uses `--features cli` only (no server deps, smaller binary).
- `install.sh` at repo root — POSIX sh script that:
  - Detects OS/arch via `uname`.
  - Downloads the matching binary from `https://github.com/<org>/vai/releases/latest/download/vai-<target>.tar.gz`.
  - Verifies SHA256 against `SHA256SUMS` file in the release.
  - Extracts and installs to `~/.local/bin/vai` or `/usr/local/bin/vai` (prompts for sudo if needed).
  - Prints success + next-step hint.
- Release workflow also generates `SHA256SUMS` by piping all tarballs through `sha256sum`.

**Detail:**
- Binary name in tarball: `vai` (no platform suffix inside the tarball).
- Tarball naming: `vai-<target-triple>.tar.gz`.
- Install.sh should **refuse to run as root** (print a message) — root install should use the sudo-prompted path, not `curl | sudo sh`.
- Install.sh should detect if `~/.local/bin` is on PATH and warn if not.

**Acceptance criteria:**
- `git tag v0.2.0 && git push --tags` produces a release with 4 tarballs + SHA256SUMS.
- `curl -fsSL https://raw.githubusercontent.com/<org>/vai/main/install.sh | sh` installs the binary end-to-end on a fresh macOS and Linux VM.
- `vai --version` prints the correct version.
- Install script fails loudly (exits non-zero) on unsupported OS/arch.

---

#### V-2: Server — open `POST /api/repos` to authenticated users

**Priority:** high
**Blocks:** V-5

Today `POST /api/repos` requires admin role. Change to allow any authenticated user, with a per-user quota.

**Files:**
- `src/server/admin.rs` (or wherever the handler lives — `POST /api/repos`). Remove admin-only gate.
- `src/server/repos.rs` or similar — add quota check via `SELECT COUNT(*) FROM repo_collaborators WHERE user_id = $1 AND role = 'admin'`.
- `src/storage/postgres/*.rs` — if needed, add helper `count_repos_owned_by_user(user_id)`.

**Detail:**
- Default quota: 100 repos per user. Configurable via `VAI_MAX_REPOS_PER_USER` env var read at server startup.
- On quota exceeded: return HTTP 403 with `{"error":"repo quota exceeded","limit":100,"current":100}`.
- On successful creation: auto-insert a row in `repo_collaborators` with `(repo_id, user_id, role='admin')`. User becomes the owner of the repo.
- The old admin-only path should still work for admins — admins can create repos without counting against a quota.

**Acceptance criteria:**
- A non-admin user with a valid API key can `POST /api/repos` and receive a 201 with the created repo.
- The user is automatically a collaborator with `admin` role.
- User is blocked at the 101st repo with a 403 referencing the quota.
- Admin can still create repos regardless of quota.
- Integration test in `tests/` covering both the success case and the quota case.
- `cargo test --features full` passes.

---

#### V-3: Server — device code flow endpoints

**Priority:** medium
**Blocks:** V-4 (device mode)

New endpoints for the `vai login --device` fallback.

**Files:**
- `src/server/auth.rs` — add three handlers:
  - `POST /api/auth/cli-device` → returns `{ code, verification_url, poll_interval }`.
  - `GET /api/auth/cli-device/:code` → returns status + api_key when authorized.
  - `POST /api/auth/cli-device/authorize` → authenticated, associates the code with a fresh API key.
- `migrations/NNNN_cli_device_codes.sql` — create `cli_device_codes` table:
  ```sql
  CREATE TABLE cli_device_codes (
      code TEXT PRIMARY KEY,
      user_id UUID REFERENCES users(id),
      api_key TEXT,
      expires_at TIMESTAMP WITH TIME ZONE NOT NULL,
      created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT now()
  );
  CREATE INDEX idx_cli_device_codes_expires ON cli_device_codes (expires_at);
  ```
- `src/storage/postgres/auth.rs` — add methods `create_device_code`, `get_device_code`, `authorize_device_code`.
- Periodic cleanup: either a simple `DELETE WHERE expires_at < now()` on every read, or a scheduled task (prefer the read-time cleanup for v1).

**Detail:**
- Code format: `XXXX-YYYY` — 4 uppercase alphanumerics, dash, 4 more. Total entropy ~40 bits, acceptable for 10-min TTL.
- TTL: 10 minutes.
- Poll interval returned: 3 seconds.
- `POST /api/auth/cli-device/authorize` mints the API key via existing creation path with name = `CLI (device code)`, role = `write`, scope = null.
- Endpoints must be OpenAPI-annotated with `#[utoipa::path]` per CLAUDE.md conventions.

**Acceptance criteria:**
- `curl -X POST /api/auth/cli-device` returns a valid code.
- Polling `GET /api/auth/cli-device/:code` returns `pending` until authorized, then `authorized` with a real key, then 404 after the key is retrieved.
- Authorizing with a non-existent or expired code returns 404.
- API key returned is usable against other endpoints.
- Integration test covering the full flow.
- `cargo test --features full` passes.

---

#### V-4: CLI — `vai login` command

**Priority:** high
**Depends on:** V-1, V-3
**Blocks:** V-5

New CLI subcommand implementing browser callback + device code fallback.

**Files:**
- `src/cli/mod.rs` — add `Login { dashboard_url: Option<String>, device: bool, name: Option<String> }` variant to `Commands` enum.
- `src/cli/login.rs` (new) — handler implementation.
- `src/credentials.rs` (new) — read/write `~/.vai/credentials.toml`.
- `Cargo.toml` — add `tiny_http` (or similar minimal HTTP server crate) for the localhost callback.

**Detail:**
- Default dashboard URL: read from `VAI_DASHBOARD_URL` env var, fall back to a compile-time constant (`https://vai.example.com` — replace before first release).
- Browser callback:
  - Bind `127.0.0.1:0` (OS-assigned ephemeral port), read back the actual port.
  - Generate 32-byte random `state`, hex-encode.
  - Open browser via `webbrowser` crate (or shell out to `open`/`xdg-open`).
  - Accept one POST to `/callback`, validate state, parse body, write credentials.
  - Timeout: 5 minutes. On timeout, print hint about `--device` and exit 1.
- Device mode:
  - Call `POST /api/auth/cli-device` on `$VAI_SERVER_URL` (from `VAI_SERVER_URL` env or embedded default).
  - Print code and verification URL, poll until authorized or expired.
- `credentials.toml` format:
  ```toml
  [default]
  server_url = "https://vai.example.com"
  api_key = "vk_live_..."
  user_id = "..."
  user_email = "..."
  ```
  Mode 0600. Parent directory created with mode 0700.
- Auto-device fallback conditions: (1) `--device` explicitly passed, (2) no `DISPLAY` / `WAYLAND_DISPLAY` on Linux, (3) opening browser fails.

**Acceptance criteria:**
- `vai login` opens the default browser to the dashboard `/cli-auth` URL.
- After approval, credentials are written to `~/.vai/credentials.toml` with mode 0600.
- Subsequent commands (`vai init`, `vai push`, etc.) can read and use these credentials.
- `vai login --device` prints a code and polls until authorized.
- Running `vai login` a second time overwrites existing credentials cleanly.
- Unit tests for credentials read/write.
- Integration test (with mock dashboard endpoint) for the browser flow.
- `cargo test` passes.

---

#### V-5: CLI — `vai init` auto-registration

**Priority:** high
**Depends on:** V-2, V-4
**Blocks:** V-7

Update `vai init` to register the repo on the server during initialization.

**Files:**
- `src/repo.rs` — extend `init()` to optionally contact the server. Add `InitOptions` struct with `local_only`, `no_push`, `remote_name` fields.
- `src/cli/mod.rs` — update `Commands::Init` to accept `--local-only`, `--no-push`, `--remote-name <name>` flags.
- `src/credentials.rs` (from V-4) — read credentials here.

**Detail:**
- After creating `.vai/`, if credentials exist and `--local-only` is not set:
  1. Infer repo name from directory basename.
  2. `POST /api/repos { name }` with `Authorization: Bearer <api_key>`.
  3. On 409 (conflict): prompt for alternative name with default `<name>-2`, then `<name>-3`, etc. Up to 3 retries.
  4. On 403 (quota): print helpful message referencing `/settings` and exit 2.
  5. On success: write `repo_id` and `remote.url` / `remote.repo_name` to `.vai/config.toml`.
  6. If payload > 100 MB after gitignore filtering, print top 5 largest paths and prompt `"Push anyway? [y/N]"`. On 'N', exit 0 with `.vai/` created but no push done.
  7. Run initial push with message `"initial commit"`.
- `.env` safety: check `.gitignore` at repo root. If `.env` is not matched, append `\n.env\n` and print `"Added .env to .gitignore."`.
- Migrate away from `config.toml` storing `api_key`: config.toml no longer has an api_key field at all (credentials come from `~/.vai/credentials.toml` or `VAI_API_KEY`).

**Acceptance criteria:**
- `vai init` in a directory with a project scaffold creates `.vai/`, registers the repo on the server, and pushes the initial snapshot.
- `vai init --local-only` creates `.vai/` without contacting the server.
- `vai init --no-push` registers but does not push.
- Collision handling: in a dir named `myapp` with a repo named `myapp` already registered, user is prompted for a new name.
- Payload warning triggers correctly for a repo with >100 MB of tracked files.
- `.env` is added to `.gitignore` if absent.
- `config.toml` no longer contains any `api_key` field.
- Integration test covering the full flow against a test server.
- `cargo test` passes.

---

#### V-6: CLI — migrate api_key storage out of `.vai/config.toml`

**Priority:** medium
**Depends on:** V-4

Remove `api_key` from `.vai/config.toml` entirely. All authentication goes through `~/.vai/credentials.toml` or `VAI_API_KEY` env var.

**Files:**
- `src/repo.rs` — update `RepoConfig` struct: remove `api_key` field from `[remote]` block.
- `src/cli/remote.rs` — update `vai remote add/remove` handlers to no longer prompt for/store api_key. If a user wants to store per-repo credentials they must use env vars or `VAI_API_KEY`.
- Migration: on `vai init` or `vai status`, if an existing `.vai/config.toml` has `api_key`, strip it, warn the user once, and write back.

**Acceptance criteria:**
- Fresh `vai init` never writes an `api_key` into config.toml.
- Existing config.toml with an api_key is stripped on the first command that rewrites the file, with a one-line warning.
- All CLI commands that talk to a server read credentials from `~/.vai/credentials.toml` or `VAI_API_KEY`.
- `vai remote add` does not prompt for an api_key.
- Unit tests updated.
- `cargo test` passes.

---

#### V-7: CLI — `vai agent loop` subcommand scaffold

**Priority:** high
**Depends on:** V-5
**Blocks:** V-8, V-9, V-10, V-11, V-12, V-13

Scaffold the new subcommand group without any templates or detection yet. Just the command tree.

**Files:**
- `src/cli/agent_cmd.rs` — add `Loop(LoopCommand)` variant. `LoopCommand` has subvariants `Init`, `Run`, `List`.
- `src/cli/agent_loop/mod.rs` (new directory) — main entry point.
- `src/cli/agent_loop/init.rs` (new) — stub that prints "not yet implemented".
- `src/cli/agent_loop/run.rs` (new) — stub.
- `src/cli/agent_loop/list.rs` (new) — stub.

**Acceptance criteria:**
- `vai agent loop --help` shows init/run/list.
- Each subcommand is callable (even if stubbed).
- `cargo test` passes.

---

#### V-8: CLI — project-type detection

**Priority:** high
**Depends on:** V-7
**Blocks:** V-9, V-11

Implement the detection logic that classifies a repo into one of `frontend-react`, `backend-rust`, `backend-typescript`, `generic`.

**Files:**
- `src/cli/agent_loop/detection.rs` (new) — function `detect_project_type(repo_root: &Path) -> ProjectType`.

**Detail:**
- `Cargo.toml` without `package.json` → `backend-rust`.
- `package.json` present: parse `dependencies` + `devDependencies` as JSON. If any of `react`, `vue`, `svelte`, `next`, `remix`, `nuxt`, `@angular/core` are present → `frontend-react`. Else → `backend-typescript`.
- Both `Cargo.toml` and `package.json` with frontend framework → `frontend-react`.
- Neither → `generic`.

**Acceptance criteria:**
- Unit tests for each branch: (1) rust-only, (2) TS without React, (3) React frontend, (4) full-stack, (5) generic empty dir.
- `cargo test` passes.

---

#### V-9: CLI — embedded prompt and agent.toml templates

**Priority:** high
**Depends on:** V-7

Ship the four project-type templates (prompt + agent.toml snippet) embedded in the binary via `include_str!`.

**Files:**
- `src/cli/agent_loop/templates/frontend-react/prompt.md` — copy vai-dashboard's `/.sandcastle/prompt.md`. Adjust any repo-specific references to be generic (replace "vai-dashboard" with a placeholder or strip).
- `src/cli/agent_loop/templates/frontend-react/agent.toml.partial` — `[checks]` block for frontend.
- `src/cli/agent_loop/templates/backend-rust/prompt.md` — three-phase workflow adapted for Rust (no Playwright, use cargo commands, emphasise unit tests and `cargo clippy`).
- `src/cli/agent_loop/templates/backend-rust/agent.toml.partial`.
- `src/cli/agent_loop/templates/backend-typescript/prompt.md`.
- `src/cli/agent_loop/templates/backend-typescript/agent.toml.partial`.
- `src/cli/agent_loop/templates/generic/prompt.md` — minimal.
- `src/cli/agent_loop/templates/generic/agent.toml.partial`.
- `src/cli/agent_loop/templates.rs` (new) — exposes a function `fn template(project_type, file_kind) -> &'static str` that returns the right include_str!.

**Detail:**
- All templates are baked into the binary at compile time. No file access at runtime.
- Templates should use `{{REPO_NAME}}` and `{{SERVER_URL}}` tokens that `agent_loop/init.rs` substitutes before writing.

**Acceptance criteria:**
- `templates::template(ProjectType::FrontendReact, TemplateKind::Prompt)` returns the React prompt.
- All four prompt templates exist and are non-empty.
- `cargo test` passes.

---

#### V-10: CLI — `.env` handling and VAI_API_KEY auto-creation

**Priority:** high
**Depends on:** V-7, V-4

Implement the `.env` manipulation and the server key creation.

**Files:**
- `src/cli/agent_loop/env.rs` (new) — parse `.env`, detect existing keys, append missing ones. Pure function, no I/O in the parser.
- `src/cli/agent_loop/env_writer.rs` (new) — write with preamble comment.
- Integration with `src/cli/agent_loop/init.rs`.

**Detail:**
- `.env` parsing is line-based. Each line is either a comment (`#...`), blank, or `KEY=VALUE`. Preserve formatting and order of existing lines.
- When adding missing keys, append a block:
  ```
  
  # --- vai loop (added 2026-04-17) ---
  VAI_API_KEY=vk_live_...
  # Claude Code OAuth token — run `claude setup-token` and paste the token here:
  CLAUDE_CODE_OAUTH_TOKEN=
  ```
- Provider-specific preamble text varies per agent choice:
  - claude-code → `# Claude Code OAuth token — run \`claude setup-token\` and paste the token here:` + `CLAUDE_CODE_OAUTH_TOKEN=`
  - codex → `# OpenAI API key — create one at https://platform.openai.com/api-keys` + `OPENAI_API_KEY=`
  - custom → `# Provider token (configure for your agent)` + no placeholder key
- Auto-create VAI_API_KEY via `POST /api/keys` with `name = "loop-<repo_name>"`, `role = "write"`, `repo_scope = <repo_id>`. Only if `.env` does not already have a non-empty `VAI_API_KEY=`.

**Acceptance criteria:**
- Unit tests for `.env` parser: blank file, comment-only, mixed, existing VAI_API_KEY, existing provider token.
- Append block is not duplicated on repeated `vai agent loop init` runs (idempotency).
- Auto-created VAI_API_KEY is correctly scoped.
- Integration test against a test vai server.
- `cargo test` passes.

---

#### V-11: CLI — loop script and Dockerfile generation

**Priority:** high
**Depends on:** V-7, V-9

Generate the actual `loop.sh` (and `Dockerfile` when `--docker`) for the chosen agent × mode combination.

**Files:**
- `src/cli/agent_loop/templates/loop-claude-code.bare.sh`
- `src/cli/agent_loop/templates/loop-claude-code.docker.sh`
- `src/cli/agent_loop/templates/loop-codex.bare.sh`
- `src/cli/agent_loop/templates/loop-codex.docker.sh`
- `src/cli/agent_loop/templates/loop-custom.sh` (no Docker variant for v1)
- `src/cli/agent_loop/templates/Dockerfile.claude-code` — based on vai-dashboard's sandcastle.
- `src/cli/agent_loop/templates/Dockerfile.codex` — stub, marked "experimental".
- `src/cli/agent_loop/generate.rs` (new) — picks the right template, substitutes tokens, writes files.

**Detail:**
- Loop scripts source `.env` at repo root, not inside `.vai/agents/<name>/`.
- Docker loop scripts use `docker run --env-file ../../../.env ...` with the correct relative path to the repo-root `.env`.
- Loop scripts are marked executable (mode 0755).
- Interactive picker for `Docker / bare` shown during init; default = Docker if `docker` is on PATH, else bare.

**Acceptance criteria:**
- Generating for each agent × mode produces a working script.
- Generated scripts pass `shellcheck`.
- Claude-code Docker script produces a working Docker image when built.
- Bare claude-code script can claim + run + submit on a test repo with an issue.
- `cargo test` passes.

---

#### V-12: CLI — `vai agent prompt` overlay concatenation

**Priority:** medium
**Depends on:** V-7

Extend the existing `vai agent prompt` command to concatenate `.vai/custom-prompt.md` between the base template and the issue JSON.

**Files:**
- `src/cli/agent_cmd.rs` — update the `prompt` subcommand handler.

**Detail:**
- Read base prompt from `.vai/agents/<name>/prompt.md` (or the existing `--template` flag path, for backwards compatibility with pre-PRD-26 repos).
- If `.vai/custom-prompt.md` exists at repo root (actually at `.vai/custom-prompt.md`), read its contents.
- Assemble: `base + "\n\n" + custom + "\n\n" + rendered_issue_json`.
- If custom-prompt.md doesn't exist, skip the middle section cleanly.
- Behaviour must stay identical for existing users who don't have custom-prompt.md (no crashes, no warnings).

**Acceptance criteria:**
- `vai agent prompt` produces the right concatenated output with and without custom-prompt.md.
- Old `.vai/prompt.md` path still works as a fallback when `.vai/agents/<name>/prompt.md` doesn't exist.
- Unit test for each concatenation path.
- `cargo test` passes.

---

#### V-13: CLI — `vai agent loop run` with pre-flight validation

**Priority:** high
**Depends on:** V-10, V-11

Implement the `run` subcommand.

**Files:**
- `src/cli/agent_loop/run.rs` — read `.vai/agent.toml`, resolve agent name (default or `--name` flag), pre-flight check env vars and Docker daemon, exec the loop script.

**Detail:**
- Pre-flight checks:
  - `.env` exists at repo root.
  - `VAI_API_KEY` non-empty in `.env`.
  - Agent-specific token non-empty (claude-code → `CLAUDE_CODE_OAUTH_TOKEN`; codex → `OPENAI_API_KEY`; custom → no check).
  - Docker mode: `docker info` returns 0.
- On failure, print the specific `.env` line number to fix.
- On success, `exec` the loop script (replaces the current process — the loop runs in the user's terminal).

**Acceptance criteria:**
- `vai agent loop run` fails with clear message when env vars are missing.
- With env vars set, it execs the loop script.
- Multiple configs in `.vai/agents/*` — `--name` picks the right one; default is the only one present, or prompts if more than one exists and no default is set in agent.toml.
- `cargo test` passes.

---

#### V-14: CLI — `vai agent loop list`

**Priority:** low
**Depends on:** V-7

Implement `vai agent loop list`.

**Files:**
- `src/cli/agent_loop/list.rs`.

**Detail:**
- List every subdirectory under `.vai/agents/`.
- For each, print: name, mode (Docker if `Dockerfile` exists, else bare), last-run timestamp (mtime of `.vai/agents/<name>/.last-run` — write this file in `run.rs` before exec).

**Acceptance criteria:**
- With zero configs, prints `No loops configured. Run 'vai agent loop init' to create one.`
- With one or more configs, prints them in a table.
- `cargo test` passes.

---

### vai-dashboard — onboarding + platform docs

#### D-1: Onboarding — `/welcome` route with live checklist

**Priority:** high
**Depends on:** V-2, V-4 (so the checklist has real things to check for)

Create the welcome page.

**Files:**
- `src/routes/welcome.tsx` — new route.
- `src/components/welcome/Checklist.tsx` — the checklist component.
- `src/components/welcome/ChecklistItem.tsx` — single item (status dot, heading, command + copy button).
- `src/hooks/use-onboarding-status.ts` — polls vai server endpoints every 3 seconds; returns `{ cli_logged_in, first_repo, first_issue, all_complete }`.

**Detail:**
- Route shown to all logged-in users at `/welcome`. If `user.onboarding_completed_at IS NOT NULL` and no `?force=1`, redirect to `/`.
- 5 checklist items (+1 optional). Each:
  - Status dot rendered via a small SVG (three states: pending circle, current dot, complete check).
  - Heading.
  - Command block (pre-wrap, monospace) with an inline copy-to-clipboard button using existing clipboard hook.
  - Optional CTA button (e.g., step 5 → "Open issue creator").
- Poll `/api/keys`, `/api/repos`, `/api/repos/{slug}/issues?limit=1` for each of the user's repos. All endpoints already exist on the vai server.
- "Skip" link bottom-right: POST to `/api/me/skip-onboarding` (new endpoint, see D-3).
- Clean visuals: match existing dashboard card aesthetic (rounded-xl, border, shadow-sm).

**Acceptance criteria:**
- `/welcome` renders with all 5 steps pending on first visit.
- Running `vai login` makes step 2 flip to complete within 5 seconds without a page refresh.
- `vai init` creating a repo flips step 3 within 5 seconds.
- Creating an issue flips step 5.
- "Skip" dismisses the page permanently.
- Copy-to-clipboard works on each command.
- Unit test for the hook and each checklist item.
- `pnpm test` passes.
- `pnpm test:e2e` passes with a new spec covering the happy path via mocked server state.

---

#### D-2: Onboarding — persist `onboarding_completed_at`

**Priority:** high
**Depends on:** D-1

Add a column to the user table and auto-set it when the checklist completes.

**Files:**
- Better Auth schema extension — add `onboarding_completed_at TIMESTAMP NULL` as an additional field on the user model in `src/lib/auth.ts`.
- `src/routes/api/me/skip-onboarding.ts` (or equivalent TanStack Start server function) — POST handler that sets the column to `now()` for the authenticated user.
- `src/hooks/use-onboarding-status.ts` (from D-1) — when all steps become complete, auto-POST to the skip endpoint to persist state.

**Detail:**
- Migration: Better Auth handles schema sync. Ensure the field lands in the Postgres session table config correctly.
- The column lives in the `user` table (Postgres), not in `session`.
- Auto-skip endpoint should be idempotent.

**Acceptance criteria:**
- Column exists on `user` after migration.
- Completing the checklist sets `onboarding_completed_at`.
- Clicking "Skip" sets `onboarding_completed_at`.
- `/welcome` redirects to `/` when set and no `?force=1`.
- Unit test for the endpoint.
- `pnpm test` passes.

---

#### D-3: `/cli-auth` and `/cli` routes for CLI login

**Priority:** high
**Depends on:** V-4 (matched to CLI-side implementation)

Create the dashboard pages the `vai login` browser and device flows land on.

**Files:**
- `src/routes/cli-auth.tsx` — browser callback approval page.
- `src/routes/cli.tsx` — device code entry page.
- `src/components/cli/AuthorizeCard.tsx` — shared approval UI.

**Detail for `/cli-auth?port=NNNN&state=<state>&hostname=<host>`:**
- Validates `port` in [49152, 65535] and `state` is a non-empty hex string.
- Requires Better Auth session (redirect to login if absent).
- Shows: "The vai CLI on `<hostname>` wants to sign in as `<your email>`. Authorize?"
- On Approve:
  - Calls vai server `POST /api/keys` (via orval-generated client, need to first validate the JWT is fresh) with `name="CLI on <hostname>"` (or just `"CLI"` if no hostname), `role="write"`, `repo_scope=null`.
  - POSTs result to `http://127.0.0.1:<port>/callback?state=<state>` with JSON body `{ api_key, user_id, user_email }`.
- On success, shows: "You can close this tab and return to your terminal."
- On failure (callback unreachable, vai server error), shows a specific error message.

**Detail for `/cli`:**
- Input for the device code.
- On submit, POSTs to vai server `POST /api/auth/cli-device/authorize` with the code.
- On success, shows "CLI authorized."

**Acceptance criteria:**
- `/cli-auth?port=49152&state=abc123` shows the approval UI.
- Approve button completes the full flow end to end against a running CLI.
- `/cli` accepts a code and authorizes it.
- Appropriate error handling for every failure mode.
- Unit tests for the two routes.
- `pnpm test:e2e` spec for at least the `/cli-auth` path with a mocked server.
- `pnpm test` passes.

---

#### D-4: Platform docs — `/help` route + content

**Priority:** high
**Depends on:** none (but coexists with PRD 05 docs viewer)

Add a top-level help section using the existing docs-viewer infrastructure.

**Files:**
- `src/routes/help.tsx` — index route.
- `src/routes/help/$.tsx` — catch-all splat route.
- `src/components/help/HelpLayout.tsx` — two-panel layout mirroring `DocViewer` but sourcing content from committed repo files.
- `docs/platform/README.md` — table of contents / "start here".
- `docs/platform/getting-started.md` — expanded onboarding guide with screenshots and troubleshooting.
- `docs/platform/planning-pattern.md` — how to use a local agent for planning + API examples.
- `docs/platform/writing-good-issues.md` — guide on acceptance criteria, scoping.
- `docs/platform/customising-prompts.md` — `.vai/agents/<name>/prompt.md` + `.vai/custom-prompt.md`.
- `docs/platform/running-multiple-loops.md` — parallel Docker loops.
- Build-time step to bundle `docs/platform/**/*.md` into the client (via Vite import glob or similar).

**Detail:**
- Content is resolved at build time, not fetched at runtime — platform docs version-lock with the dashboard version.
- `/help` renders `README.md`.
- `/help/*path` renders `<path>.md` from `docs/platform/`. 404 if not found.
- Left sidebar: static file tree over `docs/platform/`.
- Right panel: `MarkdownRenderer` with `tokens` prop (see D-5).

**Acceptance criteria:**
- `/help` loads the README.
- `/help/planning-pattern` loads the planning pattern guide.
- 404 for non-existent paths.
- All 6 content files exist and are non-empty.
- `pnpm test` passes with a new test covering route resolution.

---

#### D-5: Platform docs — dynamic token substitution in MarkdownRenderer

**Priority:** medium
**Depends on:** D-4

Extend the `MarkdownRenderer` to substitute `{{KEY}}` tokens in source before rendering.

**Files:**
- `src/components/shared/MarkdownRenderer.tsx` — add optional `tokens?: Record<string, string>` prop. Pre-process `content` to replace `{{KEY}}` with `tokens[KEY]` (leave unchanged if key absent).

**Detail:**
- Regex: `/\{\{([A-Z_][A-Z0-9_]*)\}\}/g`. Replace each match with `tokens[key] ?? full-match`.
- Apply this preprocessing before attachment-url resolution (order: token substitution → attachment resolution → ReactMarkdown).
- Existing callers of `MarkdownRenderer` pass nothing and see unchanged behaviour.
- In `HelpLayout`, default tokens = `{ VAI_URL: serverUrl, REPO: currentRepoSlug || "<repo>" }`.

**Acceptance criteria:**
- `<MarkdownRenderer content="foo {{X}} bar" tokens={{X: "baz"}} />` renders `foo baz bar`.
- Without the prop, existing behaviour is unchanged.
- Missing tokens render the literal `{{X}}`.
- Unit test covering each case.
- `pnpm test` passes.

---

#### D-6: Sidebar — "Getting Started" and "Help" entries

**Priority:** medium
**Depends on:** D-1, D-4

Add two new entries to the sidebar.

**Files:**
- `src/components/Sidebar.tsx` — add entries after "Docs".
- `src/components/Sidebar.test.tsx` — update test.

**Detail:**
- Entry 1: "Getting Started" — icon `Sparkles` or `PlayCircle` from lucide. Route: `/welcome?force=1`. Active when on `/welcome`.
- Entry 2: "Help" — icon `LifeBuoy` or `HelpCircle`. Route: `/help`. Active when pathname starts with `/help`.
- Both entries are always visible.

**Acceptance criteria:**
- Sidebar shows both entries.
- Clicking navigates correctly.
- Active state highlights the right entry.
- `pnpm test` passes.
- `pnpm test:e2e` navigation spec updated.

---

#### D-7: Repo picker empty state → link to `/welcome`

**Priority:** low
**Depends on:** D-1

Update the empty state on the home route to link to the welcome page.

**Files:**
- `src/routes/index.tsx` (or `src/routes/$repoSlug.tsx` — wherever the empty state lives).

**Detail:**
- Replace current text `"No repositories found. Start a vai server with an initialized repo."` with:
  > No repos yet. Visit Getting Started → to set up your first repo.
- CTA links to `/welcome?force=1`.

**Acceptance criteria:**
- User with zero repos sees the new empty state with a working link.
- `pnpm test` + `pnpm test:e2e` passes.

---

## Dependencies graph

```
V-1 (installer) ────────► V-4 (vai login)
V-3 (device endpoints) ─► V-4
V-2 (open POST /api/repos) ─► V-5 (vai init register)
V-4 ─► V-5
V-5 ─► V-7 (loop subcommand scaffold)
V-7 ─► V-8, V-9, V-10, V-11, V-12, V-14
V-4 ─► V-10 (env + auto-create key)
V-10, V-11 ─► V-13 (loop run pre-flight)

D-1 (welcome) ─► D-2 (persist onboarding_completed_at)
D-1 depends on V-2, V-4
D-3 (cli-auth page) depends on V-4
D-4 (/help content) independent
D-5 depends on D-4
D-6 depends on D-1, D-4
D-7 depends on D-1

V-6 (config.toml migration) can be done any time after V-4
```

Critical path is: V-1 → V-4 → V-5 → V-7 → V-10/V-11 → V-13 for the CLI. In parallel, V-2 unblocks V-5 and D-1. D-1/D-3/D-4 can start once V-2 and V-4 land.

## Out of Scope

Explicit deferrals to named follow-up PRDs:

- **Team / org UI** — dashboard-side exposure of server orgs + invites + member management. Follow-up "PRD XX: Team mode UI".
- **MCP server for planning agents** — `vai mcp serve` exposing issues/workspaces/etc. as native MCP tools. Follow-up "PRD XX: vai MCP server". The planning pattern in v1 uses manual curl examples documented in `/help/planning-pattern`.
- **Dashboard planning agent UI** — embed a planning agent chat in the dashboard. Deferred indefinitely.
- **Auto-generated `CLAUDE.md` / `AGENTS.md`** at repo root — conflicts with the `.vai/custom-prompt.md` overlay design and risks double-injection. Users can hand-edit these files; vai doesn't generate them.
- **Homebrew / cargo install / npm distribution** — install.sh covers 95% of users for v1. Follow-up if we hit adoption friction.
- **Python project-type template** — Python ecosystem has too many tooling paths (poetry, uv, pip, pytest, hatch) to pick a sane default. Deferred to v1.1 once we have user signal.
- **Aider, Cursor, and other agent templates** — claude-code and codex only in v1.
- **Email verification gate** — Better Auth supports it, not required at signup for v1.
- **Windows support** — install.sh targets macOS + Linux only. Windows users use WSL.
- **Onboarding step 4 automatic detection** — can't detect "user has run `vai agent loop init`" from the server alone in v1. The welcome page treats step 4 as implicitly complete when step 5 becomes available.

## Testing Plan

**Unit tests** (Rust + TypeScript):
- Credentials read/write (`src/credentials.rs`).
- `.env` parser and writer.
- Project-type detection (5 scenarios).
- Template token substitution.
- MarkdownRenderer token prop.
- Onboarding status hook.

**Integration tests** (Rust):
- `POST /api/repos` with a non-admin user → 201.
- 101st repo creation → 403.
- Full device code flow.
- `vai init` against a test server end-to-end.
- `vai agent loop init` + `run` smoke test for claude-code bare mode.

**E2E tests** (Playwright):
- Welcome page: all 5 checklist items flip from pending to complete as state changes.
- `/cli-auth` flow with a mocked CLI listener.
- `/help/planning-pattern` renders with correct token substitution.
- Sidebar entries clickable and navigate.

**Manual verification**:
- Fresh macOS VM: run `curl | sh`, then `vai login`, `vai init`, `vai agent loop init`, `vai agent loop run`. Time the full flow.
- Fresh Linux VM: same.
- Device code flow on a headless SSH session.

## Rollout

1. Land V-1 (installer + release workflow). Tag v0.2.0. Validate manual install.
2. Land V-2 (open repo creation) + V-3 (device endpoints). Deploy server.
3. Land V-4 (vai login) + V-6 (config migration). Tag v0.3.0.
4. Land V-5 (vai init auto-register). Tag v0.4.0.
5. Land V-7 through V-14 (loop subcommands). Tag v0.5.0.
6. Land D-3 (cli-auth page) in parallel with V-4. Deploy dashboard.
7. Land D-1, D-2 (welcome page). Deploy dashboard.
8. Land D-4, D-5 (platform docs). Deploy dashboard.
9. Land D-6, D-7 (sidebar + empty state). Deploy dashboard.
10. Manual walkthrough from scratch on macOS + Linux to validate the whole journey.
11. Public launch: update vai.dev landing page to reference `curl | sh`.

No flag-gating needed — each issue lands behind already-deployed infrastructure, and the welcome page is only visible to new users.

## Success Criteria

After this PRD lands, we should be able to:

- Hand a developer the `curl | sh` command and have them running their first RALPH loop within 10 minutes with no other assistance.
- Show the welcome page's completion rate (fraction of signups that complete all 5 steps) as a single metric for onboarding health.
- Answer "how do I onboard a new team member onto vai" by linking to `/help/getting-started`.
