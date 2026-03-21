# Phase 3 — Issue System

## Goal

Add a built-in issue tracking and work lifecycle system. Issues are first-class objects that flow through a pipeline: creation → assignment → intent → workspace → changes → merge. The system includes a smart work queue API for external orchestrators and a human escalation flow for intent conflicts.

**Depends on:** Phase 2 (Coordination) complete.

---

## PRD 3.1: Issue Lifecycle

### Summary
Issues represent units of work to be done. They can be created by humans or agents. An issue becomes an intent when an agent picks it up, connecting the planning layer to the execution layer.

### Requirements

**Functional:**
- `vai issue create --title "<text>" --description "<text>"` creates an issue
  - Issues can be created by humans via CLI or by agents via API
  - Each issue has: ID, title, description, creator (human or agent ID), priority, labels, timestamps
- `vai issue list` shows all issues with filtering:
  - `--status open|in-progress|resolved|closed`
  - `--priority critical|high|medium|low`
  - `--label <label>`
  - `--created-by <agent_id|human>`
- `vai issue show <id>` displays issue details including linked intents, workspaces, and versions
- `vai issue update <id>` modifies issue fields
- `vai issue close <id> --resolution <resolved|wontfix|duplicate>`

**Issue States:**
```
Open → In Progress → Resolved → Closed
     → Closed (won't fix / duplicate)
```

**Issue → Intent Linking:**
- When an agent creates a workspace with `--issue <issue_id>`, the issue transitions to "In Progress" and is linked to the workspace's intent
- When the workspace is merged, the issue transitions to "Resolved"
- When the workspace is discarded, the issue transitions back to "Open"
- An issue can have multiple linked workspaces (e.g., first attempt failed, second succeeded)

**Issue Events:**
```
IssueCreated { id, title, description, creator, priority }
IssueUpdated { id, fields_changed }
IssueAssigned { id, agent_id }
IssueLinkedToWorkspace { issue_id, workspace_id }
IssueResolved { id, resolution, version_id }
IssueClosed { id, resolution }
```

**API Endpoints:**
```
POST   /api/issues                 # create issue
GET    /api/issues                 # list with filters
GET    /api/issues/:id             # issue details
PATCH  /api/issues/:id             # update issue
POST   /api/issues/:id/close       # close issue
```

**Non-Functional:**
- Issue queries should return in under 50ms
- Support up to 100,000 issues per repository

### Out of Scope
- Issue templates
- Issue dependencies / blocking relationships (consider for future)
- External issue tracker sync (consider for future)

---

## PRD 3.2: Smart Work Queue

### Summary
The smart work queue is an API that returns issues safe to work on in parallel. It uses the semantic graph and active workspace data to ensure assigned work doesn't conflict. This is the primary integration point for external orchestrators.

### Requirements

**Functional:**
- `GET /api/work-queue` returns a ranked list of issues that are safe to start now:
  ```json
  {
    "available_work": [
      {
        "issue_id": "issue-123",
        "title": "add rate limiting to auth",
        "priority": "high",
        "predicted_scope": {
          "entities": ["AuthService", "RateLimiter"],
          "files": ["src/auth/service.rs", "src/auth/middleware.rs"],
          "blast_radius": 12
        },
        "conflicts_with_in_flight": [],
        "estimated_complexity": "medium"
      }
    ],
    "blocked_work": [
      {
        "issue_id": "issue-456",
        "title": "refactor auth error handling",
        "blocked_by": ["ws-abc123"],
        "reason": "Agent oauth-migration is modifying AuthService"
      }
    ]
  }
  ```
- The queue analyzes each open issue's predicted scope against all active workspaces
- Issues with no predicted conflicts are marked as available
- Issues with predicted conflicts are marked as blocked with reasons
- Available issues are ranked by priority, then by independence (fewer potential conflicts = higher rank)
- `POST /api/work-queue/claim` allows an orchestrator to claim an issue for an agent:
  - Atomically marks the issue as in-progress
  - Returns workspace creation details
  - Rejects if the issue has become conflicting since the queue was last read

**Scope Prediction:**
- For issues without explicit scope annotations, the system uses keyword matching against the semantic graph to predict which entities will be affected
- Prediction confidence is included in the response
- As more issues are resolved, prediction accuracy can be measured (actual scope vs predicted scope)

**Concurrency Safety:**
- Claiming is atomic — two orchestrators can't claim the same issue
- The queue is eventually consistent — a newly created workspace may take up to 1 second to appear in conflict analysis

**Non-Functional:**
- Queue query should return in under 500ms with 1,000 open issues and 500 active workspaces
- Queue should update within 5 seconds of workspace creation/completion

### Out of Scope
- Agent capability matching (orchestrator's responsibility)
- Agent provisioning or lifecycle management
- Priority auto-adjustment

---

## PRD 3.3: Human Escalation Flow

### Summary
When conflicts are too severe for automated resolution or agent self-repair, the system escalates to a human. The escalation presents the conflict at the intent level, not the code level, minimizing cognitive burden.

### Requirements

**Functional:**
- Escalation is triggered when:
  - The merge engine detects a Level 3 (referential) conflict that neither involved agent can resolve after one retry
  - The conflict engine detects "critical" overlap between active workspaces
  - An agent explicitly requests human review
  - A merge produces code that fails post-merge validation (parse check)

- Escalation creates an `Escalation` object:
  ```
  Escalation {
      id,
      type: MergeConflict | IntentConflict | ReviewRequest | ValidationFailure,
      severity: high | critical,
      intents_involved: [Intent],
      agents_involved: [AgentId],
      conflict_summary: String,       // human-readable
      entities_affected: [Entity],
      options: [ResolutionOption],     // suggested resolutions
      created_at,
      resolved_at: Option,
      resolution: Option,
  }
  ```

- `vai escalations list` shows pending escalations
- `vai escalations show <id>` displays full context:
  - What each agent was trying to do (intents)
  - What specifically conflicts (entities, with code snippets)
  - Suggested resolution options
  - Impact of each resolution option
- `vai escalations resolve <id> --option <n>` applies a resolution
- `vai escalations resolve <id> --custom` opens the conflict for manual editing

**Resolution Options (auto-generated):**
- "Keep Agent A's changes, discard Agent B's" (with impact analysis)
- "Keep Agent B's changes, discard Agent A's" (with impact analysis)
- "Send back to Agent A with Agent B's context"
- "Send back to Agent B with Agent A's context"
- "Pause both workspaces for manual intervention"

**Notification:**
- When an escalation is created, the human is notified via:
  - CLI: next `vai status` shows pending escalations prominently
  - API: escalation event on the WebSocket stream
  - Future: webhook to external systems (Slack, email)

**Non-Functional:**
- Escalation context should be generated within 5 seconds
- Conflict summaries should be concise — under 500 words
- The system should not produce more than 10 escalations per hour under normal operation (if it does, the conflict engine thresholds need tuning)

### Out of Scope
- Slack/email integration (future)
- Escalation auto-resolution via AI (Phase 4)
- SLA tracking on escalation resolution time

---

## PRD 3.4: Agent-Initiated Issues

### Summary
Agents can create issues autonomously based on observations — test failures, code quality problems, security vulnerabilities, etc. These issues enter the same pipeline as human-created issues.

### Requirements

**Functional:**
- Agents create issues via `POST /api/issues` with additional metadata:
  ```json
  {
    "title": "E2E test failure: auth flow regression",
    "description": "Nightly e2e test suite failure...",
    "created_by_agent": "watcher-agent-01",
    "source": {
      "type": "test_failure",
      "test_suite": "e2e-auth",
      "failure_output": "...",
      "first_seen": "2026-03-20T02:00:00Z",
      "commit_version": "v47"
    },
    "priority": "high",
    "labels": ["bug", "automated", "auth"]
  }
  ```
- Agent-created issues are tagged with `source` metadata that explains how the issue was discovered
- Agent-created issues follow the same lifecycle as human-created issues
- The work queue includes agent-created issues alongside human-created ones

**Guardrails:**
- Rate limiting: agents can create at most N issues per hour (configurable, default: 20)
- Duplicate detection: if an agent creates an issue with a title/scope very similar to an existing open issue, the system warns or rejects
- Auto-close: if an agent-created issue is not picked up within a configurable time period, it can be auto-closed with notification

**Non-Functional:**
- Issue creation via API should return in under 100ms
- Duplicate detection should run in under 500ms

### Out of Scope
- Defining what watcher agents monitor (that's the agent's concern, not vai's)
- Agent-to-agent issue assignment (goes through the work queue)
