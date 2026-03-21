//! Watcher agent registration and discovery event pipeline.
//!
//! Watcher agents monitor external systems (CI, security scanners, etc.) and
//! submit discovery events to vai. Discoveries are processed against the
//! watcher's policy and can automatically create issues.
//!
//! ## Lifecycle
//! ```text
//! register watcher → submit discovery → [duplicate check] → [create issue] → link back
//! ```
//!
//! All state lives in `.vai/watchers.db`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::event_log::EventLog;
use crate::issue::{AgentSource, IssueError, IssuePriority, IssueStore};

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors from watcher operations.
#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Event log error: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),

    #[error("Issue error: {0}")]
    Issue(#[from] IssueError),

    #[error("Watcher not found: {0}")]
    NotFound(String),

    #[error("Watcher already registered: {0}")]
    AlreadyExists(String),

    #[error("Watcher store not initialized at {0}")]
    NotInitialized(PathBuf),

    #[error("Rate limit exceeded: watcher {agent_id} has submitted {count} discoveries this hour (max {max})")]
    RateLimitExceeded { agent_id: String, count: u32, max: u32 },

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

// ── Domain types ──────────────────────────────────────────────────────────────

/// Classification of what a watcher monitors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchType {
    /// Automated test suite results.
    TestSuite,
    /// Security vulnerability scanners.
    Security,
    /// Code quality / linting tools.
    CodeQuality,
    /// Performance monitoring.
    Performance,
    /// Dependency update checkers.
    DependencyUpdates,
    /// Custom / unclassified watcher.
    Custom(String),
}

impl WatchType {
    pub fn as_str(&self) -> &str {
        match self {
            WatchType::TestSuite => "test_suite",
            WatchType::Security => "security",
            WatchType::CodeQuality => "code_quality",
            WatchType::Performance => "performance",
            WatchType::DependencyUpdates => "dependency_updates",
            WatchType::Custom(s) => s.as_str(),
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "test_suite" => WatchType::TestSuite,
            "security" => WatchType::Security,
            "code_quality" => WatchType::CodeQuality,
            "performance" => WatchType::Performance,
            "dependency_updates" => WatchType::DependencyUpdates,
            other => WatchType::Custom(other.to_string()),
        }
    }
}

/// Policy controlling whether and how discoveries auto-create issues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueCreationPolicy {
    /// When true, issues are created automatically without human approval
    /// (subject to `require_approval_above`).
    pub auto_create: bool,
    /// Maximum number of issues this watcher may create per hour.
    pub max_per_hour: u32,
    /// Issues above this priority level require human approval instead of
    /// being auto-created. `None` means auto-create all priorities.
    pub require_approval_above: Option<String>,
}

impl Default for IssueCreationPolicy {
    fn default() -> Self {
        Self {
            auto_create: false,
            max_per_hour: 5,
            require_approval_above: Some("medium".to_string()),
        }
    }
}

impl IssueCreationPolicy {
    /// Returns true if a discovery with `priority` should be auto-created.
    pub fn should_auto_create(&self, priority: &IssuePriority) -> bool {
        if !self.auto_create {
            return false;
        }
        match &self.require_approval_above {
            None => true,
            Some(threshold) => {
                // Auto-create only if priority is at or below the threshold.
                let p_rank = priority_rank(priority.as_str());
                let t_rank = priority_rank(threshold.as_str());
                p_rank <= t_rank
            }
        }
    }
}

fn priority_rank(s: &str) -> u8 {
    match s {
        "low" => 0,
        "medium" => 1,
        "high" => 2,
        "critical" => 3,
        _ => 1,
    }
}

/// Lifecycle status of a registered watcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatcherStatus {
    Active,
    Paused,
}

impl WatcherStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            WatcherStatus::Active => "active",
            WatcherStatus::Paused => "paused",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "paused" => WatcherStatus::Paused,
            _ => WatcherStatus::Active,
        }
    }
}

/// A registered watcher agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watcher {
    /// Unique agent identifier.
    pub agent_id: String,
    /// What kind of system this watcher monitors.
    pub watch_type: WatchType,
    /// Human-readable description of what this watcher monitors.
    pub description: String,
    /// Policy governing automatic issue creation.
    pub issue_creation_policy: IssueCreationPolicy,
    /// Current lifecycle status.
    pub status: WatcherStatus,
    /// When this watcher was registered.
    pub registered_at: DateTime<Utc>,
    /// When this watcher last submitted a discovery event (`None` if never).
    pub last_discovery_at: Option<DateTime<Utc>>,
    /// Total number of discovery events submitted.
    pub discovery_count: u32,
}

// ── Discovery event types ─────────────────────────────────────────────────────

/// A discovery event submitted by a watcher agent.
///
/// Each variant captures the structured data relevant to that discovery type.
/// All variants serialize to/from JSON for storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiscoveryEventKind {
    /// A test in a test suite failed.
    TestFailureDiscovered {
        suite: String,
        test_name: String,
        failure_output: String,
        /// The vai version when the failure was detected.
        version: Option<String>,
    },
    /// A security vulnerability was found.
    SecurityVulnerabilityDiscovered {
        source: String,
        severity: String,
        affected_entities: Vec<String>,
    },
    /// A code quality rule violation was found.
    CodeQualityIssueDiscovered {
        rule: String,
        entity: String,
        description: String,
    },
    /// A performance metric regressed relative to a baseline.
    PerformanceRegressionDiscovered {
        metric: String,
        baseline: f64,
        current: f64,
        version: Option<String>,
    },
    /// A newer version of a dependency is available.
    DependencyUpdateAvailable {
        package: String,
        current_version: String,
        available_version: String,
    },
}

impl DiscoveryEventKind {
    /// Returns a canonical string identifier for this event type.
    pub fn event_type(&self) -> &'static str {
        match self {
            DiscoveryEventKind::TestFailureDiscovered { .. } => "test_failure",
            DiscoveryEventKind::SecurityVulnerabilityDiscovered { .. } => "security_vulnerability",
            DiscoveryEventKind::CodeQualityIssueDiscovered { .. } => "code_quality",
            DiscoveryEventKind::PerformanceRegressionDiscovered { .. } => "performance_regression",
            DiscoveryEventKind::DependencyUpdateAvailable { .. } => "dependency_update",
        }
    }

    /// Derive a default issue title from this discovery event.
    pub fn default_title(&self) -> String {
        match self {
            DiscoveryEventKind::TestFailureDiscovered { suite, test_name, .. } =>
                format!("Test failure: {suite}/{test_name}"),
            DiscoveryEventKind::SecurityVulnerabilityDiscovered { source, severity, .. } =>
                format!("Security vulnerability ({severity}) from {source}"),
            DiscoveryEventKind::CodeQualityIssueDiscovered { rule, entity, .. } =>
                format!("Code quality: {rule} in {entity}"),
            DiscoveryEventKind::PerformanceRegressionDiscovered { metric, baseline, current, .. } =>
                format!("Performance regression: {metric} ({baseline:.2} → {current:.2})"),
            DiscoveryEventKind::DependencyUpdateAvailable { package, available_version, .. } =>
                format!("Dependency update available: {package} → {available_version}"),
        }
    }

    /// Derive a default priority for auto-created issues.
    pub fn default_priority(&self) -> IssuePriority {
        match self {
            DiscoveryEventKind::SecurityVulnerabilityDiscovered { severity, .. } => {
                match severity.to_lowercase().as_str() {
                    "critical" => IssuePriority::Critical,
                    "high" => IssuePriority::High,
                    "low" => IssuePriority::Low,
                    _ => IssuePriority::Medium,
                }
            }
            DiscoveryEventKind::PerformanceRegressionDiscovered { baseline, current, .. } => {
                // >50% regression → high; >20% → medium; else low
                if *baseline > 0.0 {
                    let pct = (current - baseline).abs() / baseline;
                    if pct > 0.5 { IssuePriority::High }
                    else if pct > 0.2 { IssuePriority::Medium }
                    else { IssuePriority::Low }
                } else {
                    IssuePriority::Medium
                }
            }
            DiscoveryEventKind::TestFailureDiscovered { .. } => IssuePriority::Medium,
            DiscoveryEventKind::CodeQualityIssueDiscovered { .. } => IssuePriority::Low,
            DiscoveryEventKind::DependencyUpdateAvailable { .. } => IssuePriority::Low,
        }
    }

    /// Return a deduplication key — two events with the same key for the same
    /// watcher describe the same ongoing problem.
    pub fn dedup_key(&self) -> String {
        match self {
            DiscoveryEventKind::TestFailureDiscovered { suite, test_name, .. } =>
                format!("test_failure::{suite}::{test_name}"),
            DiscoveryEventKind::SecurityVulnerabilityDiscovered { source, .. } =>
                format!("security::{source}"),
            DiscoveryEventKind::CodeQualityIssueDiscovered { rule, entity, .. } =>
                format!("quality::{rule}::{entity}"),
            DiscoveryEventKind::PerformanceRegressionDiscovered { metric, .. } =>
                format!("perf::{metric}"),
            DiscoveryEventKind::DependencyUpdateAvailable { package, .. } =>
                format!("dep::{package}"),
        }
    }
}

/// A persisted discovery event record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryRecord {
    /// Unique record ID.
    pub id: Uuid,
    /// Agent that submitted this event.
    pub agent_id: String,
    /// Structured event data.
    pub event: DiscoveryEventKind,
    /// When this discovery was received.
    pub received_at: DateTime<Utc>,
    /// Issue created as a result of this discovery, if any.
    pub created_issue_id: Option<Uuid>,
    /// Whether this discovery was suppressed as a duplicate.
    pub suppressed: bool,
}

// ── Result of processing a discovery ─────────────────────────────────────────

/// Outcome of submitting a discovery event.
#[derive(Debug, Serialize)]
pub struct DiscoveryOutcome {
    /// The persisted record.
    pub record: DiscoveryRecord,
    /// Issue auto-created, if any.
    pub issue_id: Option<Uuid>,
    /// Suppressed because a similar open issue already exists.
    pub suppressed: bool,
    /// Message explaining what happened.
    pub message: String,
}

// ── WatcherStore ──────────────────────────────────────────────────────────────

/// SQLite-backed storage for watcher registrations and discovery events.
///
/// Database file lives at `.vai/watchers.db`.
pub struct WatcherStore {
    conn: Connection,
}

impl WatcherStore {
    /// Open (or create) the watcher store at `<vai_dir>/watchers.db`.
    pub fn open(vai_dir: &Path) -> Result<Self, WatcherError> {
        let db_path = vai_dir.join("watchers.db");
        let conn = Connection::open(&db_path)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), WatcherError> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS watchers (
                agent_id        TEXT PRIMARY KEY,
                watch_type      TEXT NOT NULL,
                description     TEXT NOT NULL,
                policy_json     TEXT NOT NULL,
                status          TEXT NOT NULL DEFAULT 'active',
                registered_at   TEXT NOT NULL,
                last_discovery_at TEXT,
                discovery_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS discovery_records (
                id              TEXT PRIMARY KEY,
                agent_id        TEXT NOT NULL,
                event_type      TEXT NOT NULL,
                event_json      TEXT NOT NULL,
                dedup_key       TEXT NOT NULL,
                received_at     TEXT NOT NULL,
                created_issue_id TEXT,
                suppressed      INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (agent_id) REFERENCES watchers(agent_id)
            );

            CREATE INDEX IF NOT EXISTS idx_discovery_dedup
                ON discovery_records(agent_id, dedup_key, suppressed);

            CREATE TABLE IF NOT EXISTS watcher_rate_limits (
                agent_id    TEXT NOT NULL,
                hour_bucket TEXT NOT NULL,
                count       INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (agent_id, hour_bucket)
            );
        ")?;
        Ok(())
    }

    // ── Watcher management ────────────────────────────────────────────────────

    /// Register a new watcher agent.
    ///
    /// Returns an error if a watcher with the same `agent_id` already exists.
    pub fn register(&self, watcher: &Watcher) -> Result<(), WatcherError> {
        let policy_json = serde_json::to_string(&watcher.issue_creation_policy)?;
        let result = self.conn.execute(
            "INSERT INTO watchers (agent_id, watch_type, description, policy_json, status, registered_at, last_discovery_at, discovery_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &watcher.agent_id,
                watcher.watch_type.as_str(),
                &watcher.description,
                &policy_json,
                watcher.status.as_str(),
                watcher.registered_at.to_rfc3339(),
                watcher.last_discovery_at.as_ref().map(|d| d.to_rfc3339()),
                watcher.discovery_count,
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(WatcherError::AlreadyExists(watcher.agent_id.clone()))
            }
            Err(e) => Err(WatcherError::Sqlite(e)),
        }
    }

    /// List all registered watchers.
    pub fn list(&self) -> Result<Vec<Watcher>, WatcherError> {
        let mut stmt = self.conn.prepare(
            "SELECT agent_id, watch_type, description, policy_json, status, registered_at, last_discovery_at, discovery_count
             FROM watchers
             ORDER BY registered_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, u32>(7)?,
            ))
        })?;
        let mut watchers = Vec::new();
        for row in rows {
            let (agent_id, watch_type, description, policy_json, status, registered_at, last_discovery_at, discovery_count) = row?;
            let policy: IssueCreationPolicy = serde_json::from_str(&policy_json).unwrap_or_default();
            let last_disc = last_discovery_at
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|d| d.with_timezone(&Utc));
            let reg = DateTime::parse_from_rfc3339(&registered_at)
                .unwrap_or_else(|_| Utc::now().into())
                .with_timezone(&Utc);
            watchers.push(Watcher {
                agent_id,
                watch_type: WatchType::from_str(&watch_type),
                description,
                issue_creation_policy: policy,
                status: WatcherStatus::from_str(&status),
                registered_at: reg,
                last_discovery_at: last_disc,
                discovery_count,
            });
        }
        Ok(watchers)
    }

    /// Get a single watcher by agent_id.
    pub fn get(&self, agent_id: &str) -> Result<Watcher, WatcherError> {
        let result = self.conn.query_row(
            "SELECT agent_id, watch_type, description, policy_json, status, registered_at, last_discovery_at, discovery_count
             FROM watchers WHERE agent_id = ?1",
            params![agent_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, u32>(7)?,
                ))
            },
        );
        match result {
            Ok((agent_id, watch_type, description, policy_json, status, registered_at, last_discovery_at, discovery_count)) => {
                let policy: IssueCreationPolicy = serde_json::from_str(&policy_json).unwrap_or_default();
                let last_disc = last_discovery_at
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc));
                let reg = DateTime::parse_from_rfc3339(&registered_at)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc);
                Ok(Watcher {
                    agent_id,
                    watch_type: WatchType::from_str(&watch_type),
                    description,
                    issue_creation_policy: policy,
                    status: WatcherStatus::from_str(&status),
                    registered_at: reg,
                    last_discovery_at: last_disc,
                    discovery_count,
                })
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(WatcherError::NotFound(agent_id.to_string())),
            Err(e) => Err(WatcherError::Sqlite(e)),
        }
    }

    /// Set a watcher's status to `Paused`.
    pub fn pause(&self, agent_id: &str) -> Result<(), WatcherError> {
        self.set_status(agent_id, WatcherStatus::Paused)
    }

    /// Set a watcher's status to `Active`.
    pub fn resume(&self, agent_id: &str) -> Result<(), WatcherError> {
        self.set_status(agent_id, WatcherStatus::Active)
    }

    fn set_status(&self, agent_id: &str, status: WatcherStatus) -> Result<(), WatcherError> {
        let changed = self.conn.execute(
            "UPDATE watchers SET status = ?1 WHERE agent_id = ?2",
            params![status.as_str(), agent_id],
        )?;
        if changed == 0 {
            Err(WatcherError::NotFound(agent_id.to_string()))
        } else {
            Ok(())
        }
    }

    // ── Discovery processing ──────────────────────────────────────────────────

    /// Submit a discovery event from a watcher.
    ///
    /// Runs the full pipeline:
    /// 1. Validate the watcher exists and is active.
    /// 2. Rate-limit check.
    /// 3. Duplicate suppression.
    /// 4. Persist the discovery record.
    /// 5. Auto-create an issue if the watcher policy allows.
    pub fn submit_discovery(
        &self,
        agent_id: &str,
        event: DiscoveryEventKind,
        issue_store: &IssueStore,
        event_log: &mut EventLog,
    ) -> Result<DiscoveryOutcome, WatcherError> {
        // Step 1: validate watcher.
        let watcher = self.get(agent_id)?;
        if watcher.status == WatcherStatus::Paused {
            return Err(WatcherError::NotFound(format!(
                "{agent_id} is paused — resume before submitting discoveries"
            )));
        }

        // Step 2: rate-limit check.
        let count = self.increment_rate_limit(agent_id)?;
        if count > watcher.issue_creation_policy.max_per_hour {
            let _ = self.decrement_rate_limit(agent_id);
            return Err(WatcherError::RateLimitExceeded {
                agent_id: agent_id.to_string(),
                count,
                max: watcher.issue_creation_policy.max_per_hour,
            });
        }

        let dedup_key = event.dedup_key();
        let now = Utc::now();

        // Step 3: duplicate suppression.
        // Check if an open issue was already created from the same dedup key
        // for this watcher and no version has changed since.
        let existing_issue_id = self.find_active_discovery(&dedup_key, agent_id)?;

        let record_id = Uuid::new_v4();
        let event_json = serde_json::to_string(&event)?;

        if let Some(issue_id) = existing_issue_id {
            // Suppress — same problem already has an open issue.
            self.conn.execute(
                "INSERT INTO discovery_records (id, agent_id, event_type, event_json, dedup_key, received_at, created_issue_id, suppressed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
                params![
                    record_id.to_string(),
                    agent_id,
                    event.event_type(),
                    &event_json,
                    &dedup_key,
                    now.to_rfc3339(),
                    issue_id.to_string(),
                ],
            )?;
            self.update_watcher_stats(agent_id, &now)?;

            let record = DiscoveryRecord {
                id: record_id,
                agent_id: agent_id.to_string(),
                event,
                received_at: now,
                created_issue_id: Some(issue_id),
                suppressed: true,
            };
            return Ok(DiscoveryOutcome {
                record,
                issue_id: None,
                suppressed: true,
                message: format!("Suppressed duplicate: issue {issue_id} already tracks this problem"),
            });
        }

        // Step 4: decide whether to auto-create an issue.
        let priority = event.default_priority();
        let should_create = watcher.issue_creation_policy.should_auto_create(&priority);

        let mut created_issue_id: Option<Uuid> = None;
        if should_create {
            let title = event.default_title();
            let description = format!(
                "Automatically created by watcher `{agent_id}`.\n\n**Event type:** {}\n\n**Details:**\n```json\n{}\n```",
                event.event_type(),
                serde_json::to_string_pretty(&event).unwrap_or_default(),
            );
            let source = AgentSource {
                source_type: event.event_type().to_string(),
                details: serde_json::to_value(&event).unwrap_or_default(),
            };
            match issue_store.create_by_agent(
                title,
                description,
                priority,
                vec![event.event_type().to_string(), "watcher".to_string()],
                agent_id,
                source,
                watcher.issue_creation_policy.max_per_hour,
                event_log,
            ) {
                Ok((issue, _dup)) => {
                    created_issue_id = Some(issue.id);
                }
                Err(IssueError::RateLimitExceeded { .. }) => {
                    // Issue rate limit hit — record discovery without issue.
                }
                Err(e) => return Err(WatcherError::Issue(e)),
            }
        }

        // Step 5: persist the discovery record.
        self.conn.execute(
            "INSERT INTO discovery_records (id, agent_id, event_type, event_json, dedup_key, received_at, created_issue_id, suppressed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
            params![
                record_id.to_string(),
                agent_id,
                event.event_type(),
                &event_json,
                &dedup_key,
                now.to_rfc3339(),
                created_issue_id.as_ref().map(|id| id.to_string()),
            ],
        )?;
        self.update_watcher_stats(agent_id, &now)?;

        let message = if let Some(issue_id) = created_issue_id {
            format!("Discovery recorded; issue {issue_id} created")
        } else if should_create {
            "Discovery recorded; issue creation rate-limited".to_string()
        } else {
            "Discovery recorded; auto-create disabled by policy".to_string()
        };

        let record = DiscoveryRecord {
            id: record_id,
            agent_id: agent_id.to_string(),
            event,
            received_at: now,
            created_issue_id,
            suppressed: false,
        };
        Ok(DiscoveryOutcome {
            record,
            issue_id: created_issue_id,
            suppressed: false,
            message,
        })
    }

    /// List recent discovery records (most recent first).
    pub fn list_discoveries(
        &self,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DiscoveryRecord>, WatcherError> {
        let limit = limit.min(1000) as i64;
        type Row = (String, String, String, String, Option<String>, bool);
        let rows: Vec<Row> = if let Some(aid) = agent_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, agent_id, event_json, received_at, created_issue_id, suppressed
                 FROM discovery_records WHERE agent_id = ?1
                 ORDER BY received_at DESC LIMIT ?2",
            )?;
            let collected: Vec<Row> = stmt.query_map(params![aid, limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?.collect::<Result<_, _>>()?;
            collected
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, agent_id, event_json, received_at, created_issue_id, suppressed
                 FROM discovery_records
                 ORDER BY received_at DESC LIMIT ?1",
            )?;
            let collected: Vec<Row> = stmt.query_map(params![limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?.collect::<Result<_, _>>()?;
            collected
        };

        let mut records = Vec::new();
        for (id_str, agent_id, event_json, received_at, created_issue_id, suppressed) in rows {
            let id = Uuid::parse_str(&id_str).unwrap_or_default();
            let event: DiscoveryEventKind = serde_json::from_str(&event_json).unwrap_or(
                DiscoveryEventKind::TestFailureDiscovered {
                    suite: "unknown".to_string(),
                    test_name: "unknown".to_string(),
                    failure_output: String::new(),
                    version: None,
                },
            );
            let received = DateTime::parse_from_rfc3339(&received_at)
                .unwrap_or_else(|_| Utc::now().into())
                .with_timezone(&Utc);
            let issue_id = created_issue_id.and_then(|s| Uuid::parse_str(&s).ok());
            records.push(DiscoveryRecord {
                id,
                agent_id,
                event,
                received_at: received,
                created_issue_id: issue_id,
                suppressed,
            });
        }
        Ok(records)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Find an existing non-suppressed discovery with the same dedup key that
    /// produced an open issue (i.e. the problem is already being tracked).
    fn find_active_discovery(&self, dedup_key: &str, agent_id: &str) -> Result<Option<Uuid>, WatcherError> {
        let result: rusqlite::Result<String> = self.conn.query_row(
            "SELECT created_issue_id FROM discovery_records
             WHERE agent_id = ?1 AND dedup_key = ?2 AND suppressed = 0 AND created_issue_id IS NOT NULL
             ORDER BY received_at DESC LIMIT 1",
            params![agent_id, dedup_key],
            |row| row.get(0),
        );
        match result {
            Ok(id_str) => Ok(Uuid::parse_str(&id_str).ok()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(WatcherError::Sqlite(e)),
        }
    }

    fn increment_rate_limit(&self, agent_id: &str) -> Result<u32, WatcherError> {
        let bucket = current_hour_bucket();
        self.conn.execute(
            "INSERT INTO watcher_rate_limits (agent_id, hour_bucket, count)
             VALUES (?1, ?2, 1)
             ON CONFLICT(agent_id, hour_bucket) DO UPDATE SET count = count + 1",
            params![agent_id, &bucket],
        )?;
        let count: u32 = self.conn.query_row(
            "SELECT count FROM watcher_rate_limits WHERE agent_id = ?1 AND hour_bucket = ?2",
            params![agent_id, &bucket],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    fn decrement_rate_limit(&self, agent_id: &str) -> Result<(), WatcherError> {
        let bucket = current_hour_bucket();
        self.conn.execute(
            "UPDATE watcher_rate_limits SET count = MAX(0, count - 1)
             WHERE agent_id = ?1 AND hour_bucket = ?2",
            params![agent_id, &bucket],
        )?;
        Ok(())
    }

    fn update_watcher_stats(&self, agent_id: &str, ts: &DateTime<Utc>) -> Result<(), WatcherError> {
        self.conn.execute(
            "UPDATE watchers SET last_discovery_at = ?1, discovery_count = discovery_count + 1
             WHERE agent_id = ?2",
            params![ts.to_rfc3339(), agent_id],
        )?;
        Ok(())
    }
}

/// Returns the current hour as a string bucket, e.g. `"2026031415"` (YYYYMMDDhh).
fn current_hour_bucket() -> String {
    Utc::now().format("%Y%m%d%H").to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::event_log::EventLog;
    use crate::issue::IssueStore;

    fn setup() -> (TempDir, WatcherStore, IssueStore, EventLog) {
        let dir = tempfile::tempdir().unwrap();
        let vai_dir = dir.path();
        let ws = WatcherStore::open(vai_dir).unwrap();
        let is = IssueStore::open(vai_dir).unwrap();
        let el = EventLog::open(vai_dir).unwrap();
        (dir, ws, is, el)
    }

    fn sample_watcher(agent_id: &str) -> Watcher {
        Watcher {
            agent_id: agent_id.to_string(),
            watch_type: WatchType::TestSuite,
            description: "Monitors CI tests".to_string(),
            issue_creation_policy: IssueCreationPolicy {
                auto_create: true,
                max_per_hour: 10,
                require_approval_above: Some("high".to_string()),
            },
            status: WatcherStatus::Active,
            registered_at: Utc::now(),
            last_discovery_at: None,
            discovery_count: 0,
        }
    }

    #[test]
    fn test_register_and_list() {
        let (_dir, ws, _, _) = setup();
        let w = sample_watcher("ci-agent");
        ws.register(&w).unwrap();

        let list = ws.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].agent_id, "ci-agent");
        assert_eq!(list[0].status, WatcherStatus::Active);
    }

    #[test]
    fn test_register_duplicate_fails() {
        let (_dir, ws, _, _) = setup();
        let w = sample_watcher("ci-agent");
        ws.register(&w).unwrap();
        assert!(matches!(ws.register(&w), Err(WatcherError::AlreadyExists(_))));
    }

    #[test]
    fn test_pause_resume() {
        let (_dir, ws, _, _) = setup();
        ws.register(&sample_watcher("ci-agent")).unwrap();
        ws.pause("ci-agent").unwrap();
        assert_eq!(ws.get("ci-agent").unwrap().status, WatcherStatus::Paused);
        ws.resume("ci-agent").unwrap();
        assert_eq!(ws.get("ci-agent").unwrap().status, WatcherStatus::Active);
    }

    #[test]
    fn test_submit_discovery_creates_issue() {
        let (_dir, ws, is, mut el) = setup();
        ws.register(&sample_watcher("ci-agent")).unwrap();

        let event = DiscoveryEventKind::TestFailureDiscovered {
            suite: "unit".to_string(),
            test_name: "test_login".to_string(),
            failure_output: "assertion failed".to_string(),
            version: Some("v1".to_string()),
        };

        let outcome = ws.submit_discovery("ci-agent", event, &is, &mut el).unwrap();
        assert!(!outcome.suppressed);
        assert!(outcome.issue_id.is_some());
    }

    #[test]
    fn test_submit_discovery_duplicate_suppressed() {
        let (_dir, ws, is, mut el) = setup();
        ws.register(&sample_watcher("ci-agent")).unwrap();

        let event = DiscoveryEventKind::TestFailureDiscovered {
            suite: "unit".to_string(),
            test_name: "test_login".to_string(),
            failure_output: "assertion failed".to_string(),
            version: Some("v1".to_string()),
        };

        let first = ws.submit_discovery("ci-agent", event.clone(), &is, &mut el).unwrap();
        assert!(!first.suppressed);

        let second = ws.submit_discovery("ci-agent", event, &is, &mut el).unwrap();
        assert!(second.suppressed);
    }

    #[test]
    fn test_paused_watcher_rejects_discovery() {
        let (_dir, ws, is, mut el) = setup();
        ws.register(&sample_watcher("ci-agent")).unwrap();
        ws.pause("ci-agent").unwrap();

        let event = DiscoveryEventKind::TestFailureDiscovered {
            suite: "unit".to_string(),
            test_name: "test_x".to_string(),
            failure_output: String::new(),
            version: None,
        };

        assert!(ws.submit_discovery("ci-agent", event, &is, &mut el).is_err());
    }

    #[test]
    fn test_policy_auto_create_false() {
        let (_dir, ws, is, mut el) = setup();
        let mut w = sample_watcher("passive-agent");
        w.issue_creation_policy.auto_create = false;
        ws.register(&w).unwrap();

        let event = DiscoveryEventKind::TestFailureDiscovered {
            suite: "e2e".to_string(),
            test_name: "test_y".to_string(),
            failure_output: String::new(),
            version: None,
        };

        let outcome = ws.submit_discovery("passive-agent", event, &is, &mut el).unwrap();
        assert!(!outcome.suppressed);
        assert!(outcome.issue_id.is_none());
    }

    #[test]
    fn test_priority_rank_ordering() {
        let policy = IssueCreationPolicy {
            auto_create: true,
            max_per_hour: 10,
            require_approval_above: Some("medium".to_string()),
        };
        // Low and medium should auto-create.
        assert!(policy.should_auto_create(&IssuePriority::Low));
        assert!(policy.should_auto_create(&IssuePriority::Medium));
        // High and critical require approval.
        assert!(!policy.should_auto_create(&IssuePriority::High));
        assert!(!policy.should_auto_create(&IssuePriority::Critical));
    }
}
