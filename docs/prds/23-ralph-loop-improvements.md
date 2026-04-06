# PRD: RALPH Agent Loop Improvements

## Status

Proposed

## Context

The RALPH agent loop works but has quality gaps. Agents submit code that passes unit tests but breaks e2e tests, introduces auth race conditions, or loses file permissions — bugs that would be caught if the agent could see and test the running application before submitting.

This PRD improves the loop in four areas:

1. **Verify setup/teardown** — build the app and start servers before running checks
2. **Three-phase prompt** — explore with MCP, implement, verify visually
3. **Red-green TDD** — run e2e tests before and after implementation
4. **E2e tests in verify** — catch regressions before submit

## Design Decisions

### Verify Setup/Teardown in agent.toml

Add `setup` and `teardown` arrays to `[checks]` in agent.toml:

```toml
[checks]
setup = [
    "pnpm build",
    "pnpm preview --port 3001 &",
    "sleep 3",
]
commands = [
    "npx biome check --write src/",
    "npx tsc --noEmit",
    "pnpm run test",
    "pnpm test:e2e",
]
teardown = [
    "kill %1 2>/dev/null || true",
]
```

**Behavior:**

1. `setup` commands run sequentially before checks
2. If any setup command fails (non-zero exit), checks are **skipped** and the setup error is returned as the verify failure — the agent sees the build error and can fix it
3. `commands` run as today — each checked for pass/fail
4. `teardown` runs **always** (even if setup or commands fail) — cleanup background processes
5. Setup stdout/stderr is captured and included in error output on failure

This keeps the agent.toml declarative. The `vai agent verify` implementation handles the lifecycle.

### Three-Phase Prompt

Update the prompt template to instruct the agent to work in three phases:

**Phase 1: Explore** — before writing any code, use Playwright MCP to navigate to the relevant pages. Take screenshots, inspect the DOM, understand the current state. This prevents the agent from guessing at selectors and UI structure.

**Phase 2: Implement** — write the code changes. For UI issues, write or update e2e tests first (red), then implement the fix (green).

**Phase 3: Verify visually** — after implementation, use Playwright MCP to navigate to the affected pages and verify the changes look correct. Take screenshots. Check for console errors. This catches visual regressions the automated tests might miss.

The prompt template change is in `.vai/prompt.md` (or `.sandcastle/prompt.md` for the dashboard).

### Red-Green TDD Pattern

For issues that touch UI components or routes:

1. Agent writes or identifies the relevant e2e test
2. Runs it — expects it to fail (red) confirming the test catches the issue
3. Implements the fix
4. Runs it again — expects it to pass (green)
5. Proceeds to verify

This is enforced by prompt instructions, not code. The agent has access to `pnpm test:e2e --grep "pattern"` to run specific tests.

### E2E Tests in Verify

With the setup/teardown infrastructure, e2e tests become part of the verify gate. The dashboard's agent.toml setup builds the app and starts a preview server, then `pnpm test:e2e` runs against it.

For the vai-dashboard specifically:

```toml
[checks]
setup = [
    "pnpm install --frozen-lockfile",
    "pnpm build",
    "pnpm preview --port 3001 &",
    "sleep 3",
]
commands = [
    "npx biome check --write src/",
    "npx tsc --noEmit",
    "pnpm run test",
    "DASHBOARD_URL=http://localhost:3001 pnpm test:e2e",
]
teardown = [
    "kill %1 2>/dev/null || true",
]
```

Note: `DASHBOARD_URL` overrides the e2e config to point at the preview server instead of the dev server.

## Issue Breakdown

### Issue 1: Implement verify setup/teardown in vai agent verify

**Priority:** high
**Blocks:** Issues 3, 4

Add `setup` and `teardown` support to `vai agent verify`.

**Files:**
- `src/agent/mod.rs` — update `AgentConfig` to parse `checks.setup` and `checks.teardown` arrays
- `src/agent/mod.rs` — update `verify()` function:
  1. Run setup commands sequentially, capture output
  2. If setup fails: skip checks, return setup error as `CheckResult` with the failed command's output
  3. Run check commands as today
  4. Run teardown commands always (ignore errors)
- Update `VerifyResult` to distinguish setup failures from check failures

**Config format:**
```toml
[checks]
setup = ["pnpm build", "pnpm preview --port 3001 &", "sleep 3"]
commands = ["npx tsc --noEmit", "pnpm test"]
teardown = ["kill %1 2>/dev/null || true"]
```

**Backward compatible:** If `setup`/`teardown` are absent, behavior is unchanged.

**Acceptance criteria:**
- Setup commands run before checks
- Setup failure skips checks and returns the error to the caller
- Teardown runs always
- `cargo test` passes
- Existing agent.toml without setup/teardown still works

---

### Issue 2: Update dashboard prompt template for three-phase workflow

**Priority:** high

Update `.sandcastle/prompt.md` to instruct the agent to work in three phases.

**Changes to prompt template:**

Add after the conventions section:

```markdown
## Workflow

Work in three phases:

### Phase 1: Explore
Before writing any code, use Playwright MCP to understand the current state:
- Navigate to the pages affected by this issue
- Take screenshots to see the current UI
- Use browser_snapshot to inspect the DOM structure
- Note the actual element names, classes, and hierarchy

Do NOT skip this step. Do NOT guess at selectors or UI structure.

### Phase 2: Implement
Now implement the changes:
- For UI changes: write or update the e2e test first, run it to confirm it fails (red)
- Implement the fix
- Run the e2e test again to confirm it passes (green)
- Run all quality checks

### Phase 3: Verify Visually
After implementation, verify your work:
- Navigate to the affected pages with Playwright MCP
- Take screenshots to confirm the UI looks correct
- Check the browser console for errors
- If something looks wrong, fix it before finishing
```

**Acceptance criteria:**
- Prompt template includes three-phase instructions
- Prompt is clear about using MCP before writing code

---

### Issue 3: Update dashboard agent.toml with setup/teardown and e2e checks

**Priority:** high
**Depends on:** Issue 1

Update the vai-dashboard agent.toml to use the new setup/teardown and include e2e tests.

**Changes to `.vai/agent.toml`:**
```toml
[checks]
setup = [
    "pnpm install --frozen-lockfile",
    "pnpm build",
    "pnpm preview --port 3001 &",
    "sleep 3",
]
commands = [
    "npx biome check --write src/",
    "npx tsc --noEmit",
    "pnpm run test",
    "DASHBOARD_URL=http://localhost:3001 pnpm test:e2e",
]
teardown = [
    "kill %1 2>/dev/null || true",
]
```

This needs to be pushed to the vai server so RALPH picks it up.

**Acceptance criteria:**
- `vai agent verify ./work` builds the app, starts preview, runs all checks including e2e
- Build failures are caught and reported
- Preview server is cleaned up after verify
- E2e tests run against the preview server

---

### Issue 4: Smoke test the full loop — create a test issue and verify the three-phase workflow

**Priority:** medium
**Depends on:** Issues 1, 2, 3

Create a small test issue on vai-dashboard and run one RALPH iteration to verify the full improved loop works:

- Agent explores with MCP (phase 1)
- Implements with red-green TDD (phase 2)
- Verifies visually (phase 3)
- Setup/teardown runs correctly
- E2e tests are part of verify
- Build failures would block submit

**Acceptance criteria:**
- One full RALPH iteration completes successfully with the new loop
- Agent output shows MCP usage in phases 1 and 3
- Verify output shows setup → checks (including e2e) → teardown
- No regressions in existing functionality
