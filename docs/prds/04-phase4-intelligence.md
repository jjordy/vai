# Phase 4 — Intelligence

## Goal

Add intelligent capabilities: automatic scope inference from natural language intents, improved merge resolution through learned patterns, and a TUI dashboard for human oversight. These features leverage the data accumulated from Phases 1-3 to make the system progressively smarter.

**Depends on:** Phase 3 (Issue System) complete.

---

## PRD 4.1: Automatic Scope Inference

### Summary
When an agent or human describes an intent in natural language, the system automatically predicts which semantic entities will be affected. This powers the conflict engine's proactive conflict prevention and the work queue's parallel-safety analysis.

### Requirements

**Functional:**
- Given a natural language intent (e.g., "add rate limiting to auth endpoints"), the system:
  1. Extracts key terms and concepts from the intent text
  2. Matches against the semantic graph: entity names, qualified names, file paths, module names
  3. Expands matches via graph relationships (if `AuthService` is matched, include its methods and callers)
  4. Returns a predicted scope with confidence scores:
     ```
     Predicted scope for "add rate limiting to auth endpoints":
       HIGH confidence:
         - AuthService (direct name match)
         - auth_middleware() (related entity)
       MEDIUM confidence:
         - RateLimiter (inferred from "rate limiting")
         - src/api/routes.rs (contains auth endpoint handlers)
       LOW confidence:
         - DatabasePool (transitive dependency of AuthService)
     ```
- The predicted scope is shown to the agent at workspace creation for confirmation
- The agent can accept, adjust, or override the predicted scope
- As workspaces complete, the system records predicted vs actual scope for accuracy tracking

**Inference Strategy (Phase 4 MVP):**
- Keyword extraction + semantic graph search (no LLM dependency)
- Term matching against entity names, file paths, comments
- Graph expansion: N-hop traversal from matched entities
- Historical patterns: if past intents mentioning "auth" touched entities X, Y, Z, weight those higher

**Future Enhancement:**
- LLM-assisted scope inference for ambiguous intents
- Learning from correction patterns (agent adjusts predicted scope → update model)

**Non-Functional:**
- Scope inference should return in under 2 seconds
- Prediction accuracy target: 70%+ of actually-touched entities are in the predicted scope (recall)
- False positive rate under 30% (precision)

### Out of Scope
- Natural language understanding of intent *conflicts* (e.g., understanding that "migrate to OAuth2" and "add features to basic auth" are contradictory)
- Cross-repository scope inference

---

## PRD 4.2: Merge Intelligence

### Summary
The merge engine learns from resolved conflicts to improve future auto-resolution. Over time, common conflict patterns are recognized and resolved automatically rather than escalated.

### Requirements

**Functional:**
- Every conflict resolution (whether by agent or human) is recorded with:
  - The conflict pattern (what overlapped, at what level)
  - The resolution strategy chosen
  - Whether the resolution was successful (no subsequent rollback or fix needed)
- The system builds a **conflict pattern library**:
  ```
  Pattern: "Two agents add imports to same file"
  Historical resolution: "Merge both imports, deduplicate" (success rate: 98%)
  → Auto-resolve future instances

  Pattern: "One agent renames identifier, another adds usage of old name"
  Historical resolution: "Update new usage to use new name" (success rate: 85%)
  → Auto-resolve with post-merge validation

  Pattern: "Two agents modify same function body differently"
  Historical resolution: varies widely
  → Continue escalating
  ```
- Patterns with >90% historical success rate and >10 instances can be promoted to auto-resolution
- Auto-resolved-via-pattern merges are flagged in the event log for auditability
- `vai merge patterns` shows learned patterns and their success rates
- Humans can override: `vai merge patterns disable <pattern_id>` to prevent auto-resolution of specific patterns

**Feedback Loop:**
- If an auto-resolved merge is subsequently rolled back, the pattern's success rate decreases
- If the success rate drops below threshold, the pattern is demoted back to manual resolution
- This creates a self-correcting system

**Non-Functional:**
- Pattern matching should add no more than 100ms to merge time
- Pattern library should support up to 1,000 patterns

### Out of Scope
- LLM-assisted merge resolution
- Cross-repository pattern sharing

---

## PRD 4.3: TUI Dashboard

### Summary
A terminal-based dashboard for human oversight of the vai system. Provides real-time visibility into agent activity, workspace status, conflicts, and system health. Designed to minimize cognitive burden — the human sees intents and summaries, not code diffs.

### Requirements

**Functional:**

**Main Dashboard View:**
```
╔══════════════════════════════════════════════════════════════╗
║ vai dashboard — myproject                    v47 │ 12 agents ║
╠══════════════════════════════════════════════════════════════╣
║                                                              ║
║  ACTIVE WORK                                                 ║
║  ┌──────────────────────────────────────────────────────┐    ║
║  │ ● Agent-A  "migrate auth to OAuth2"      ████░░ 60%  │    ║
║  │ ● Agent-B  "fix token expiry"            ██████ done  │    ║
║  │ ● Agent-C  "add observability"           ███░░░ 45%   │    ║
║  │ ○ Agent-D  "rate limiting"               ░░░░░░ queued│    ║
║  └──────────────────────────────────────────────────────┘    ║
║                                                              ║
║  CONFLICTS (1)                              ISSUES           ║
║  ┌─────────────────────────────┐  ┌─────────────────────┐   ║
║  │ ⚠ Agent-A ↔ Agent-C         │  │ Open: 14            │   ║
║  │   AuthService overlap        │  │ In Progress: 4      │   ║
║  │   Severity: MEDIUM           │  │ Resolved today: 7   │   ║
║  │   [View details]             │  │ Agent-created: 3    │   ║
║  └─────────────────────────────┘  └─────────────────────┘   ║
║                                                              ║
║  RECENT VERSIONS                                             ║
║  v47  "add search indexing"           Agent-E    10m ago     ║
║  v46  "fix pagination bug"            Agent-F    25m ago     ║
║  v45  "refactor database layer"       Agent-G    1h ago      ║
║                                                              ║
╚══════════════════════════════════════════════════════════════╝
```

**Dashboard Panels:**
- **Active Work:** all in-progress workspaces with agent, intent, and progress indicator
- **Conflicts:** pending overlaps and escalations requiring attention
- **Issues:** summary counts by status
- **Recent Versions:** last N merged versions
- **System Health:** connected agents, event throughput, merge success rate

**Interactive Features:**
- Navigate between panels with keyboard
- Drill into a workspace to see entity-level changes
- Drill into a conflict to see escalation details and resolve
- Drill into an issue to see full details
- Filter active work by label, agent, or entity

**Commands:**
- `vai dashboard` launches the TUI in local mode (reads from `.vai/`)
- `vai dashboard --server vai://host:port` connects to a remote server
- Dashboard auto-refreshes via WebSocket event stream

**Non-Functional:**
- Dashboard should render within 500ms of launch
- Real-time updates should appear within 1 second of the underlying event
- Should work in standard 80x24 terminal but optimize for larger terminals
- Accessible via SSH (no browser required)

### Out of Scope
- Web-based dashboard (future — the TUI covers the MVP)
- Mobile interface
- Customizable dashboard layouts

---

## PRD 4.4: Agent-Initiated Discovery

### Summary
Formalize the pattern of "watcher agents" that monitor external systems and create issues in vai. Define the integration points and event types that make this pattern first-class.

### Requirements

**Functional:**

**Watcher Registration:**
- Agents can register as watchers via API:
  ```json
  {
    "agent_id": "watcher-e2e-tests",
    "watch_type": "test_suite",
    "description": "Monitors nightly e2e test results",
    "issue_creation_policy": {
      "auto_create": true,
      "max_per_hour": 5,
      "require_approval_above": "medium"
    }
  }
  ```
- Registered watchers appear in the dashboard under system health
- Watchers can be paused/resumed by humans: `vai watchers pause <agent_id>`

**Discovery Event Types:**
```
TestFailureDiscovered { suite, test_name, failure_output, version }
SecurityVulnerabilityDiscovered { source, severity, affected_entities }
CodeQualityIssueDiscovered { rule, entity, description }
PerformanceRegressionDiscovered { metric, baseline, current, version }
DependencyUpdateAvailable { package, current_version, available_version }
```

**Discovery → Issue Pipeline:**
- Discovery events can automatically create issues based on the watcher's policy
- Issues created from discoveries link back to the discovery event
- If a discovery maps to entities in the semantic graph, the issue is pre-scoped
- Duplicate discovery suppression: if the same test has been failing since the last version change, don't create duplicate issues

**Non-Functional:**
- Discovery event processing should complete within 1 second
- Duplicate detection should have <5% false negative rate

### Out of Scope
- Building specific watcher agents (vai provides the integration points, not the agents themselves)
- External system integrations (CI/CD, monitoring — watchers handle this)
