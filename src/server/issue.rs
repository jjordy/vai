//! Issue, comment, link, and attachment API handlers.
//!
//! Endpoints:
//!   - `POST /api/repos/:repo/issues` — create issue
//!   - `GET /api/repos/:repo/issues` — list issues (paginated, filterable)
//!   - `GET /api/repos/:repo/issues/:id` — full issue detail
//!   - `PATCH /api/repos/:repo/issues/:id` — update issue fields
//!   - `POST /api/repos/:repo/issues/:id/close` — close issue
//!   - `POST /api/repos/:repo/issues/:id/comments` — add comment
//!   - `GET /api/repos/:repo/issues/:id/comments` — list comments
//!   - `PATCH /api/repos/:repo/issues/:id/comments/:comment_id` — edit comment
//!   - `DELETE /api/repos/:repo/issues/:id/comments/:comment_id` — soft-delete comment
//!   - `POST /api/repos/:repo/issues/:id/links` — create issue link
//!   - `GET /api/repos/:repo/issues/:id/links` — list links
//!   - `DELETE /api/repos/:repo/issues/:id/links/:target_id` — remove link
//!   - `POST /api/repos/:repo/issues/:id/attachments` — upload attachment
//!   - `GET /api/repos/:repo/issues/:id/attachments` — list attachment metadata
//!   - `GET /api/repos/:repo/issues/:id/attachments/:filename` — download attachment
//!   - `DELETE /api/repos/:repo/issues/:id/attachments/:filename` — delete attachment

use std::collections::HashMap;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use axum::extract::{Extension, FromRequest as _, Path as AxumPath, Query as AxumQuery, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::event_log::EventKind;
use crate::storage::ListQuery;

use super::pagination::PaginatedResponse;
use super::{
    AgentIdentity, ApiError, AppState, AuthSource, BroadcastEvent, ErrorBody, PathId, RepoCtx,
    require_repo_permission, validate_str_len,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum length for issue titles (characters).
const MAX_ISSUE_TITLE_LEN: usize = 500;

/// Maximum length for issue description bodies (bytes).
const MAX_ISSUE_BODY_LEN: usize = 50 * 1024; // 50 KB

/// Maximum length for a single label (characters).
const MAX_LABEL_LEN: usize = 100;

/// Maximum number of labels per issue.
const MAX_LABELS_PER_ISSUE: usize = 20;

/// Maximum file size for issue attachments (10 MiB).
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Maximum number of attachments per issue.
const MAX_ATTACHMENTS_PER_ISSUE: usize = 10;

// ── Request / response types ──────────────────────────────────────────────────

/// Request body for `POST /api/issues`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CreateIssueRequest {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_priority")]
    priority: String,
    #[serde(default)]
    labels: Vec<String>,
    /// Human username or agent ID creating this issue.
    #[serde(default = "default_creator")]
    creator: String,
    /// When set, the issue is created on behalf of an agent with guardrails.
    /// The value is the agent's ID.
    created_by_agent: Option<String>,
    /// Discovery metadata for agent-created issues.
    source: Option<AgentSourceRequest>,
    /// Max issues this agent may create per hour (default: 20).
    #[serde(default = "default_max_per_hour")]
    max_per_hour: u32,
    /// Issue IDs that block this issue (creates `blocks` links where blocker → this issue).
    #[serde(default)]
    blocked_by: Vec<String>,
    /// Testable conditions that define when the issue is complete.
    #[serde(default)]
    acceptance_criteria: Vec<String>,
}

/// Agent discovery source passed via the REST API.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct AgentSourceRequest {
    source_type: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    details: serde_json::Value,
}

fn default_priority() -> String {
    "medium".to_string()
}

fn default_creator() -> String {
    "api".to_string()
}

fn default_max_per_hour() -> u32 {
    20
}

/// Request body for `PATCH /api/issues/:id`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct UpdateIssueRequest {
    title: Option<String>,
    description: Option<String>,
    priority: Option<String>,
    labels: Option<Vec<String>>,
    /// Testable conditions that define when the issue is complete.
    acceptance_criteria: Option<Vec<String>>,
    /// Add blockers: issue IDs that block this issue (appends; does not remove existing).
    #[serde(default)]
    blocked_by: Vec<String>,
}

/// Request body for `POST /api/issues/:id/close`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CloseIssueRequest {
    /// Resolution: "resolved", "wontfix", or "duplicate".
    resolution: String,
}

/// Query parameters for `GET /api/issues`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ListIssuesQuery {
    status: Option<String>,
    priority: Option<String>,
    label: Option<String>,
    created_by: Option<String>,
    /// Filter: only show issues blocked by this issue ID.
    blocked_by: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
    sort: Option<String>,
}

/// Response body for issue endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct IssueResponse {
    id: String,
    title: String,
    description: String,
    status: String,
    priority: String,
    labels: Vec<String>,
    creator: String,
    resolution: Option<String>,
    /// Present when the issue was created by an agent.
    #[schema(value_type = Option<Object>)]
    agent_source: Option<serde_json::Value>,
    /// Set on creation responses when a similar open issue was detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    possible_duplicate_of: Option<String>,
    linked_workspace_ids: Vec<String>,
    /// IDs of issues that block this one (source blocks this issue).
    blocked_by: Vec<String>,
    /// IDs of issues that this issue blocks (this issue is source, others are target).
    blocking: Vec<String>,
    /// Testable conditions that define when the issue is complete.
    acceptance_criteria: Vec<String>,
    created_at: String,
    updated_at: String,
}

impl IssueResponse {
    fn from_issue(
        issue: crate::issue::Issue,
        linked: Vec<uuid::Uuid>,
        blocked_by: Vec<uuid::Uuid>,
        blocking: Vec<uuid::Uuid>,
    ) -> Self {
        let agent_source = issue.agent_source.as_ref().map(|s| {
            serde_json::to_value(s).unwrap_or(serde_json::Value::Null)
        });
        IssueResponse {
            id: issue.id.to_string(),
            title: issue.title,
            description: issue.description,
            status: issue.status.as_str().to_string(),
            priority: issue.priority.as_str().to_string(),
            labels: issue.labels,
            creator: issue.creator,
            resolution: issue.resolution,
            agent_source,
            possible_duplicate_of: None,
            linked_workspace_ids: linked.iter().map(|u| u.to_string()).collect(),
            blocked_by: blocked_by.iter().map(|id| id.to_string()).collect(),
            blocking: blocking.iter().map(|id| id.to_string()).collect(),
            acceptance_criteria: issue.acceptance_criteria,
            created_at: issue.created_at.to_rfc3339(),
            updated_at: issue.updated_at.to_rfc3339(),
        }
    }
}

/// Enriched link entry used in the issue detail response.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct IssueLinkDetailResponse {
    /// UUID of the other issue in the relationship.
    other_issue_id: String,
    /// Relationship from this issue's perspective (e.g. `"blocks"`, `"is-blocked-by"`,
    /// `"relates-to"`, `"duplicates"`, `"is-duplicated-by"`).
    relationship: String,
    /// Title of the linked issue.
    title: String,
    /// Current status of the linked issue (e.g. `"open"`, `"closed"`).
    status: String,
}

/// A resolved @mention embedded in a comment response.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct MentionRef {
    /// Stable UUID — user ID for humans, API key ID for agents.
    id: String,
    /// Display name of the mentioned user or agent.
    name: String,
    /// `"human"` for users, `"agent"` for API keys.
    mention_type: String,
}

impl From<&crate::storage::CommentMention> for MentionRef {
    fn from(m: &crate::storage::CommentMention) -> Self {
        MentionRef {
            id: m.entity_id().map(|u| u.to_string()).unwrap_or_default(),
            name: m.mentioned_name.clone(),
            mention_type: m.mention_type.clone(),
        }
    }
}

/// Response body for a single issue comment.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct CommentResponse {
    id: String,
    issue_id: String,
    author: String,
    /// Comment body. `null` when the comment has been soft-deleted.
    body: Option<String>,
    /// Whether the author is a `"human"` or `"agent"`.
    author_type: String,
    /// Optional structured author identifier.
    author_id: Option<String>,
    created_at: String,
    /// Parent comment UUID for threaded replies.
    parent_id: Option<String>,
    /// When the comment was last edited, if ever.
    edited_at: Option<String>,
    /// When the comment was soft-deleted, if ever.
    deleted_at: Option<String>,
    /// Resolved @mentions found in the comment body.
    mentions: Vec<MentionRef>,
}

impl CommentResponse {
    /// Build a response from a comment and its resolved mentions.
    pub(super) fn with_mentions(c: crate::issue::IssueComment, mentions: &[crate::storage::CommentMention]) -> Self {
        CommentResponse {
            id: c.id.to_string(),
            issue_id: c.issue_id.to_string(),
            author: c.author,
            body: c.body,
            author_type: c.author_type,
            author_id: c.author_id,
            created_at: c.created_at.to_rfc3339(),
            parent_id: c.parent_id.map(|u| u.to_string()),
            edited_at: c.edited_at.map(|t| t.to_rfc3339()),
            deleted_at: c.deleted_at.map(|t| t.to_rfc3339()),
            mentions: mentions.iter().map(MentionRef::from).collect(),
        }
    }
}

impl From<crate::issue::IssueComment> for CommentResponse {
    fn from(c: crate::issue::IssueComment) -> Self {
        CommentResponse::with_mentions(c, &[])
    }
}

/// Request body for `POST /api/repos/:repo/issues/:id/comments`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CreateCommentRequest {
    /// Comment body (Markdown supported).
    body: String,
    /// Display name for the comment author. When using admin-key auth this
    /// value is used directly; for API-key and JWT auth the identity name is
    /// used instead.
    #[serde(default)]
    author: Option<String>,
    /// Author type override (`"human"` or `"agent"`). When provided with
    /// admin-key auth this value is stored as-is; otherwise it is derived from
    /// the authentication source.
    #[serde(default)]
    author_type: Option<String>,
    /// Optional structured author identifier (e.g. an agent instance ID).
    /// Stored as-is when provided.
    #[serde(default)]
    author_id: Option<String>,
    /// Optional parent comment UUID for threaded replies.
    #[serde(default)]
    parent_id: Option<String>,
}

/// Request body for `PATCH /api/repos/:repo/issues/:id/comments/:comment_id`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct UpdateCommentRequest {
    /// New comment body (Markdown supported).
    body: String,
}

/// Request body for `POST /api/repos/:repo/issues/:id/links`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CreateIssueLinkRequest {
    /// UUID of the issue to link to.
    target_id: String,
    /// Relationship from this issue to the target: `"blocks"`, `"relates-to"`, or `"duplicates"`.
    relationship: String,
}

/// Response body for a single issue link as seen from one issue's perspective.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct IssueLinkResponse {
    /// The other issue in the relationship.
    other_issue_id: String,
    /// Relationship from this issue's perspective (e.g. `"blocks"`, `"is-blocked-by"`,
    /// `"relates-to"`, `"duplicates"`, `"is-duplicated-by"`).
    relationship: String,
}

/// Request body for `POST .../attachments` — JSON upload with base64 content.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct UploadAttachmentRequest {
    /// Original filename (no path separators allowed).
    filename: String,
    /// MIME content type, e.g. `"image/png"`. Defaults to `"application/octet-stream"`.
    #[serde(default = "default_attachment_content_type")]
    content_type: String,
    /// File bytes, Base64-encoded (standard encoding).
    content: String,
    /// Username or agent ID uploading the file. Defaults to `"unknown"`.
    #[serde(default = "default_attachment_uploaded_by")]
    uploaded_by: String,
}

fn default_attachment_content_type() -> String {
    "application/octet-stream".to_string()
}

fn default_attachment_uploaded_by() -> String {
    "unknown".to_string()
}

/// Metadata response for a single issue attachment.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct AttachmentResponse {
    id: String,
    issue_id: String,
    filename: String,
    content_type: String,
    size_bytes: i64,
    uploaded_by: String,
    created_at: String,
}

impl From<crate::issue::IssueAttachment> for AttachmentResponse {
    fn from(a: crate::issue::IssueAttachment) -> Self {
        AttachmentResponse {
            id: a.id.to_string(),
            issue_id: a.issue_id.to_string(),
            filename: a.filename,
            content_type: a.content_type,
            size_bytes: a.size_bytes,
            uploaded_by: a.uploaded_by,
            created_at: a.created_at.to_rfc3339(),
        }
    }
}

/// Full issue detail response returned by `GET /api/issues/:id`.
///
/// Extends the basic issue fields with linked issues (including status),
/// file attachments, and the 50 most recent comments.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct IssueDetailResponse {
    id: String,
    title: String,
    description: String,
    status: String,
    priority: String,
    labels: Vec<String>,
    creator: String,
    resolution: Option<String>,
    /// Present when the issue was created by an agent.
    #[schema(value_type = Option<Object>)]
    agent_source: Option<serde_json::Value>,
    /// Set on creation responses when a similar open issue was detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    possible_duplicate_of: Option<String>,
    linked_workspace_ids: Vec<String>,
    /// IDs of issues that block this one (source blocks this issue).
    blocked_by: Vec<String>,
    /// IDs of issues that this issue blocks (this issue is source, others are target).
    blocking: Vec<String>,
    /// Testable conditions that define when the issue is complete.
    acceptance_criteria: Vec<String>,
    created_at: String,
    updated_at: String,
    /// All links from/to this issue with relationship type, title, and status of the other issue.
    links: Vec<IssueLinkDetailResponse>,
    /// File attachments on this issue.
    attachments: Vec<AttachmentResponse>,
    /// The 50 most recent comments on this issue.
    comments: Vec<CommentResponse>,
}

// ── Input validation helpers ──────────────────────────────────────────────────

/// Validates a list of labels: at most `MAX_LABELS_PER_ISSUE`, each at most
/// `MAX_LABEL_LEN` characters.
fn validate_labels(labels: &[String]) -> Result<(), ApiError> {
    if labels.len() > MAX_LABELS_PER_ISSUE {
        return Err(ApiError::bad_request(format!(
            "too many labels: {}, maximum is {MAX_LABELS_PER_ISSUE}",
            labels.len()
        )));
    }
    for label in labels {
        validate_str_len(label, MAX_LABEL_LEN, "label")?;
    }
    Ok(())
}

/// Returns `Err` if `filename` contains path separators or starts with `.`.
fn validate_attachment_filename(filename: &str) -> Result<(), ApiError> {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.starts_with('.')
    {
        Err(ApiError::bad_request(format!("invalid filename: `{filename}`")))
    } else {
        Ok(())
    }
}

// ── Issue helper functions ────────────────────────────────────────────────────

/// Returns the IDs of workspaces linked to `issue_id` via their `issue_id` field.
///
/// Uses the storage trait so the lookup works for both SQLite and Postgres.
/// Falls back to an empty list on error so callers never fail on this auxiliary query.
async fn linked_workspace_ids(
    ctx: &RepoCtx,
    issue_id: uuid::Uuid,
) -> Vec<uuid::Uuid> {
    ctx.storage
        .workspaces()
        .list_workspaces(&ctx.repo_id, true, &ListQuery::default())
        .await
        .map(|r| r.items)
        .unwrap_or_default()
        .into_iter()
        .filter(|ws| ws.issue_id == Some(issue_id))
        .map(|ws| ws.id)
        .collect()
}

/// Returns `(blocked_by, blocking)` for an issue from the `issue_links` table.
///
/// - `blocked_by`: IDs of issues that have a `blocks` link targeting `issue_id`.
/// - `blocking`: IDs of issues that `issue_id` has a `blocks` link targeting.
async fn links_for_issue(
    ctx: &RepoCtx,
    issue_id: uuid::Uuid,
) -> (Vec<uuid::Uuid>, Vec<uuid::Uuid>) {
    let links = ctx.storage
        .links()
        .list_links(&ctx.repo_id, &issue_id)
        .await
        .unwrap_or_default();

    let mut blocked_by = Vec::new();
    let mut blocking = Vec::new();

    for link in links {
        if link.relationship == crate::storage::IssueLinkRelationship::Blocks {
            if link.target_id == issue_id {
                // source blocks this issue
                blocked_by.push(link.source_id);
            } else if link.source_id == issue_id {
                // this issue blocks target
                blocking.push(link.target_id);
            }
        }
    }

    (blocked_by, blocking)
}

/// Extracts unique @mention names from a comment body.
///
/// Matches the pattern `@word` where the name starts with a word character and
/// may contain word characters, dots, and dashes. Duplicate names are removed.
fn extract_mention_names(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            i += 1;
            // First char must be alphanumeric or underscore.
            if i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'_'
                        || bytes[i] == b'.'
                        || bytes[i] == b'-')
                {
                    i += 1;
                }
                if let Ok(name) = std::str::from_utf8(&bytes[start..i]) {
                    names.push(name.to_string());
                }
            }
        } else {
            i += 1;
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Validates @mention names against repo members and returns `NewCommentMention` records.
///
/// Names that do not match any repo member are silently ignored.
async fn resolve_comment_mentions(
    storage: &crate::storage::StorageBackend,
    repo_id: &uuid::Uuid,
    names: Vec<String>,
) -> Vec<crate::storage::NewCommentMention> {
    if names.is_empty() {
        return vec![];
    }
    let mut result = Vec::new();
    let orgs = storage.orgs();
    for name in names {
        // Search returns prefix matches; we filter for exact case-insensitive match.
        if let Ok(members) = orgs.search_repo_members(repo_id, &name, 10).await {
            if let Some(m) = members.into_iter().find(|m| m.name.eq_ignore_ascii_case(&name)) {
                let (user_id, key_id) = if m.member_type == "human" {
                    (uuid::Uuid::parse_str(&m.id).ok(), None)
                } else {
                    (None, uuid::Uuid::parse_str(&m.id).ok())
                };
                result.push(crate::storage::NewCommentMention {
                    mentioned_user_id: user_id,
                    mentioned_key_id: key_id,
                    mentioned_name: m.name,
                    mention_type: m.member_type,
                });
            }
        }
    }
    result
}

/// Returns `true` if the authenticated identity is the author of the comment.
fn is_comment_author(identity: &AgentIdentity, comment: &crate::issue::IssueComment) -> bool {
    match &comment.author_id {
        None => identity.is_admin,
        Some(author_id) => match identity.auth_source {
            AuthSource::Jwt => {
                let my_id = identity.user_id.map(|u| u.to_string())
                    .unwrap_or_else(|| identity.key_id.clone());
                my_id == *author_id
            }
            AuthSource::ApiKey => identity.key_id == *author_id,
            AuthSource::AdminKey => identity.is_admin,
        },
    }
}

/// Extracts `(filename, content_type, bytes, uploaded_by)` from either a
/// `multipart/form-data` request or a JSON body with base64-encoded content.
async fn parse_attachment_body(
    request: axum::extract::Request,
) -> Result<(String, String, Vec<u8>, String), ApiError> {
    let ct = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if ct.contains("multipart/form-data") {
        let mut mp = axum::extract::Multipart::from_request(request, &())
            .await
            .map_err(|e| ApiError::bad_request(format!("multipart error: {e}")))?;

        let mut filename: Option<String> = None;
        let mut file_ct: Option<String> = None;
        let mut bytes: Option<Vec<u8>> = None;
        let mut uploaded_by: Option<String> = None;

        while let Some(field) = mp
            .next_field()
            .await
            .map_err(|e| ApiError::bad_request(format!("multipart field error: {e}")))?
        {
            match field.name() {
                Some("file") => {
                    if filename.is_none() {
                        filename = field.file_name().map(String::from);
                    }
                    if file_ct.is_none() {
                        file_ct = field.content_type().map(String::from);
                    }
                    let data = field
                        .bytes()
                        .await
                        .map_err(|e| ApiError::bad_request(format!("read field: {e}")))?;
                    bytes = Some(data.to_vec());
                }
                Some("filename") => {
                    let val = field
                        .text()
                        .await
                        .map_err(|e| ApiError::bad_request(format!("read field: {e}")))?;
                    filename = Some(val);
                }
                Some("content_type") => {
                    let val = field
                        .text()
                        .await
                        .map_err(|e| ApiError::bad_request(format!("read field: {e}")))?;
                    file_ct = Some(val);
                }
                Some("uploaded_by") => {
                    let val = field
                        .text()
                        .await
                        .map_err(|e| ApiError::bad_request(format!("read field: {e}")))?;
                    uploaded_by = Some(val);
                }
                _ => {}
            }
        }

        Ok((
            filename.ok_or_else(|| ApiError::bad_request("missing filename in multipart"))?,
            file_ct.unwrap_or_else(|| "application/octet-stream".to_string()),
            bytes.ok_or_else(|| ApiError::bad_request("missing file content in multipart"))?,
            uploaded_by.unwrap_or_else(|| "unknown".to_string()),
        ))
    } else {
        // JSON with base64 content.
        let body = axum::body::to_bytes(request.into_body(), MAX_ATTACHMENT_BYTES * 2)
            .await
            .map_err(|e| ApiError::bad_request(format!("read body: {e}")))?;
        let req: UploadAttachmentRequest = serde_json::from_slice(&body)
            .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
        let data = BASE64
            .decode(&req.content)
            .map_err(|e| ApiError::bad_request(format!("invalid base64: {e}")))?;
        Ok((req.filename, req.content_type, data, req.uploaded_by))
    }
}

// ── Issue CRUD handlers ───────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues",
    request_body = CreateIssueRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 201, description = "Issue created", body = IssueResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "Rate limit exceeded"),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/issues` — create a new issue.
///
/// When `created_by_agent` is set the request is treated as an agent-initiated
/// issue and goes through rate-limiting and duplicate-detection guardrails.
/// If the rate limit is exceeded the handler returns **429 Too Many Requests**
/// with a `Retry-After` header.  When a similar open issue is detected the
/// issue is still created but the response includes `possible_duplicate_of`.
///
/// Returns 201 Created with the issue metadata.
pub(super) async fn create_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<CreateIssueRequest>,
) -> Result<(StatusCode, Json<IssueResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    validate_str_len(&body.title, MAX_ISSUE_TITLE_LEN, "title")?;
    validate_str_len(&body.description, MAX_ISSUE_BODY_LEN, "description")?;
    validate_labels(&body.labels)?;
    use crate::issue::{AgentSource, IssueFilter, IssuePriority};
    use crate::storage::NewIssue;

    let _lock = state.repo_lock.lock().await;

    let priority = IssuePriority::from_db_str(&body.priority).ok_or_else(|| {
        ApiError::bad_request(format!("unknown priority `{}`", body.priority))
    })?;

    let issues = ctx.storage.issues();

    let (creator, agent_source, possible_duplicate_id) =
        if let Some(ref agent_id) = body.created_by_agent {
            // Agent-initiated path: apply rate-limiting and duplicate-detection.
            let all_issues = issues
                .list_issues(&ctx.repo_id, &IssueFilter::default(), &ListQuery::default())
                .await
                .map_err(ApiError::from)?
                .items;

            // Rate-limiting: count issues created by this agent in the last hour.
            let one_hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
            let agent_count = all_issues
                .iter()
                .filter(|i| {
                    i.creator == *agent_id
                        && i.created_at > one_hour_ago
                })
                .count() as u32;

            if agent_count >= body.max_per_hour {
                return Err(ApiError::rate_limited(format!(
                    "agent `{agent_id}` has created {agent_count} issues in the last hour \
                     (limit: {})",
                    body.max_per_hour
                )));
            }

            // Duplicate detection: Jaccard similarity on open-issue titles.
            let dup_id = crate::issue::find_similar_open_issue(&all_issues, &body.title);

            let source = body.source.as_ref().map(|s| AgentSource {
                source_type: s.source_type.clone(),
                details: s.details.clone(),
            }).unwrap_or_else(|| AgentSource {
                source_type: "unknown".into(),
                details: serde_json::Value::Null,
            });

            (agent_id.clone(), Some(source), dup_id)
        } else {
            // Human-initiated path: no guardrails.
            (body.creator.clone(), None, None)
        };

    // Parse and validate blocked_by IDs (each blocker must exist).
    let mut blocker_ids: Vec<uuid::Uuid> = Vec::new();
    for blocker_str in &body.blocked_by {
        let blocker_id = uuid::Uuid::parse_str(blocker_str)
            .map_err(|_| ApiError::bad_request(format!("invalid blocker ID `{blocker_str}`")))?;
        // Verify the blocker exists.
        ctx.storage.issues()
            .get_issue(&ctx.repo_id, &blocker_id)
            .await
            .map_err(|_| ApiError::bad_request(format!("blocker issue `{blocker_id}` not found")))?;
        blocker_ids.push(blocker_id);
    }

    let new_issue = NewIssue {
        title: body.title.clone(),
        description: body.description.clone(),
        priority,
        labels: body.labels.clone(),
        creator,
        agent_source: agent_source.map(|s| {
            serde_json::to_value(s).unwrap_or(serde_json::Value::Null)
        }),
        acceptance_criteria: body.acceptance_criteria.clone(),
    };

    let issue = issues
        .create_issue(&ctx.repo_id, new_issue)
        .await
        .map_err(ApiError::from)?;

    let issue_id = issue.id;
    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::IssueCreated {
            issue_id,
            title: issue.title.clone(),
            creator: issue.creator.clone(),
            priority: issue.priority.as_str().to_string(),
        })
        .await;

    state.broadcast(BroadcastEvent {
        event_type: "IssueCreated".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: issue.created_at.to_rfc3339(),
        data: serde_json::json!({
            "issue_id": issue_id.to_string(),
            "title": issue.title.clone(),
        }),
    });

    // Create `blocks` links for each blocker: source=blocker, target=new issue.
    for blocker_id in &blocker_ids {
        let _ = ctx.storage.links()
            .create_link(
                &ctx.repo_id,
                blocker_id,
                crate::storage::NewIssueLink {
                    target_id: issue_id,
                    relationship: crate::storage::IssueLinkRelationship::Blocks,
                },
            )
            .await;
    }

    let mut resp = IssueResponse::from_issue(issue, vec![], blocker_ids, vec![]);
    resp.possible_duplicate_of = possible_duplicate_id.map(|id| id.to_string());

    tracing::info!(
        event = "issue.created",
        actor = %identity.name,
        repo = %ctx.repo_id,
        issue_id = %issue_id,
        "issue created"
    );
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/issues` — list issues with optional filters and pagination.
///
/// Supports pagination via `?page=1&per_page=25` and sorting via
/// `?sort=created_at:desc`.  Sortable columns: `created_at`, `updated_at`,
/// `priority`, `status`, `title`.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("status" = Option<String>, Query, description = "Filter by status (open, in_progress, closed)"),
        ("priority" = Option<String>, Query, description = "Filter by priority"),
        ("label" = Option<String>, Query, description = "Filter by label"),
        ("created_by" = Option<String>, Query, description = "Filter by creator"),
        ("blocked_by" = Option<String>, Query, description = "Filter: only issues blocked by this issue ID"),
        ("page" = Option<u32>, Query, description = "Page number (1-indexed, default 1)"),
        ("per_page" = Option<u32>, Query, description = "Items per page (default 25, max 100)"),
        ("sort" = Option<String>, Query, description = "Sort fields, e.g. `created_at:desc,priority:asc`"),
    ),
    responses(
        (status = 200, description = "Paginated list of issues", body = PaginatedResponse<IssueResponse>),
        (status = 400, description = "Invalid filter, pagination, or sort params", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
pub(super) async fn list_issues_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(query): AxumQuery<ListIssuesQuery>,
) -> Result<Json<PaginatedResponse<IssueResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    use crate::issue::{IssueFilter, IssueStatus, IssuePriority};

    let status: Option<Vec<IssueStatus>> = match query.status.as_deref() {
        None => None,
        Some("open") => Some(vec![IssueStatus::Open, IssueStatus::InProgress, IssueStatus::Resolved]),
        Some(s) => Some(vec![IssueStatus::from_db_str(s)
            .ok_or_else(|| ApiError::bad_request(format!("unknown status `{s}`")))?]),
    };
    let priority = query.priority.as_deref()
        .map(|p| IssuePriority::from_db_str(p).ok_or_else(|| ApiError::bad_request(format!("unknown priority `{p}`"))))
        .transpose()?;

    let blocked_by_id = query.blocked_by.as_deref()
        .map(|b| uuid::Uuid::parse_str(b).map_err(|_| ApiError::bad_request(format!("invalid blocked_by ID `{b}`"))))
        .transpose()?;

    let filter = IssueFilter {
        status,
        priority,
        label: query.label,
        creator: query.created_by,
        blocked_by: blocked_by_id,
    };

    const ALLOWED_SORT: &[&str] = &["created_at", "updated_at", "priority", "status", "title", "creator", "id"];
    let list_query = ListQuery::from_params(
        query.page,
        query.per_page,
        query.sort.as_deref(),
        ALLOWED_SORT,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let result = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &filter, &list_query)
        .await
        .map_err(ApiError::from)?;

    // Fetch all workspaces once to compute linked workspace IDs per issue.
    let all_workspaces = ctx.storage.workspaces()
        .list_workspaces(&ctx.repo_id, true, &ListQuery::default())
        .await
        .map(|r| r.items)
        .unwrap_or_default();

    let mut response = Vec::with_capacity(result.items.len());
    for issue in result.items {
        let linked: Vec<uuid::Uuid> = all_workspaces
            .iter()
            .filter(|ws| ws.issue_id == Some(issue.id))
            .map(|ws| ws.id)
            .collect();
        let (blocked_by, blocking) = links_for_issue(&ctx, issue.id).await;
        response.push(IssueResponse::from_issue(issue, linked, blocked_by, blocking));
    }

    Ok(Json(PaginatedResponse::new(response, result.total, &list_query)))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Issue ID"),
    ),
    responses(
        (status = 200, description = "Issue details", body = IssueDetailResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/issues/:id` — full issue detail with links, attachments, and comments.
///
/// Returns a single enriched response containing the issue's metadata, all linked
/// issues (with relationship type and current status), file attachments, and the
/// 50 most recent comments.  Returns 404 if the issue does not exist.
pub(super) async fn get_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<IssueDetailResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    let issue = ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    let linked = linked_workspace_ids(&ctx, issue_id).await;
    let (blocked_by, blocking) = links_for_issue(&ctx, issue_id).await;

    // Fetch raw links, attachments, and comments concurrently.
    let links_store = ctx.storage.links();
    let attachments_store = ctx.storage.attachments();
    let comments_store = ctx.storage.comments();
    let (raw_links, attachments, all_comments, mentions_by_comment) = tokio::join!(
        links_store.list_links(&ctx.repo_id, &issue_id),
        attachments_store.list_attachments(&ctx.repo_id, &issue_id),
        comments_store.list_comments(&ctx.repo_id, &issue_id),
        comments_store.list_issue_mentions(&ctx.repo_id, &issue_id),
    );
    let raw_links = raw_links.map_err(ApiError::from)?;
    let attachments = attachments.map_err(ApiError::from)?;
    let mut all_comments = all_comments.map_err(ApiError::from)?;
    let mut mentions_by_comment = mentions_by_comment.unwrap_or_default();

    // Enrich links: fetch status + title of the other issue in each link.
    let mut links: Vec<IssueLinkDetailResponse> = Vec::with_capacity(raw_links.len());
    for link in &raw_links {
        let (other_id, relationship_str) = if link.source_id == issue_id {
            (link.target_id, link.relationship.as_str().to_string())
        } else {
            (link.source_id, link.relationship.inverse_str().to_string())
        };
        // Best-effort: if the linked issue can't be fetched, skip it.
        if let Ok(other) = ctx.storage.issues().get_issue(&ctx.repo_id, &other_id).await {
            links.push(IssueLinkDetailResponse {
                other_issue_id: other_id.to_string(),
                relationship: relationship_str,
                title: other.title,
                status: other.status.as_str().to_string(),
            });
        }
    }

    // Return the 50 most recent comments (list_comments returns oldest-first).
    let comments_start = all_comments.len().saturating_sub(50);
    let recent_comments: Vec<CommentResponse> = all_comments
        .drain(comments_start..)
        .map(|c| {
            let id = c.id;
            let mentions = mentions_by_comment.remove(&id).unwrap_or_default();
            CommentResponse::with_mentions(c, &mentions)
        })
        .collect();

    let agent_source = issue.agent_source.as_ref().map(|s| {
        serde_json::to_value(s).unwrap_or(serde_json::Value::Null)
    });

    Ok(Json(IssueDetailResponse {
        id: issue.id.to_string(),
        title: issue.title,
        description: issue.description,
        status: issue.status.as_str().to_string(),
        priority: issue.priority.as_str().to_string(),
        labels: issue.labels,
        creator: issue.creator,
        resolution: issue.resolution,
        agent_source,
        possible_duplicate_of: None,
        linked_workspace_ids: linked.iter().map(|u| u.to_string()).collect(),
        blocked_by: blocked_by.iter().map(|id| id.to_string()).collect(),
        blocking: blocking.iter().map(|id| id.to_string()).collect(),
        acceptance_criteria: issue.acceptance_criteria,
        created_at: issue.created_at.to_rfc3339(),
        updated_at: issue.updated_at.to_rfc3339(),
        links,
        attachments: attachments.into_iter().map(AttachmentResponse::from).collect(),
        comments: recent_comments,
    }))
}

#[utoipa::path(
    patch,
    path = "/api/repos/{repo}/issues/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Issue ID"),
    ),
    request_body = UpdateIssueRequest,
    responses(
        (status = 200, description = "Updated issue", body = IssueResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `PATCH /api/issues/:id` — update mutable fields of an issue.
///
/// Returns 404 if the issue does not exist.
pub(super) async fn update_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<UpdateIssueRequest>,
) -> Result<Json<IssueResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    if let Some(ref title) = body.title {
        validate_str_len(title, MAX_ISSUE_TITLE_LEN, "title")?;
    }
    if let Some(ref desc) = body.description {
        validate_str_len(desc, MAX_ISSUE_BODY_LEN, "description")?;
    }
    if let Some(ref labels) = body.labels {
        validate_labels(labels)?;
    }
    use crate::issue::IssuePriority;
    use crate::storage::IssueUpdate;

    let _lock = state.repo_lock.lock().await;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    let priority = body.priority.as_deref()
        .map(|p| IssuePriority::from_db_str(p).ok_or_else(|| ApiError::bad_request(format!("unknown priority `{p}`"))))
        .transpose()?;

    // Collect changed field names before moving body fields into update.
    let fields_changed: Vec<String> = [
        body.title.as_ref().map(|_| "title"),
        body.description.as_ref().map(|_| "description"),
        priority.as_ref().map(|_| "priority"),
        body.labels.as_ref().map(|_| "labels"),
        body.acceptance_criteria.as_ref().map(|_| "acceptance_criteria"),
    ]
    .into_iter()
    .flatten()
    .map(String::from)
    .collect();

    let update = IssueUpdate {
        title: body.title,
        description: body.description,
        priority,
        labels: body.labels,
        acceptance_criteria: body.acceptance_criteria,
        ..Default::default()
    };

    // Parse and validate any new blocked_by IDs (each blocker must exist).
    let mut new_blocker_ids: Vec<uuid::Uuid> = Vec::new();
    for blocker_str in &body.blocked_by {
        let blocker_id = uuid::Uuid::parse_str(blocker_str)
            .map_err(|_| ApiError::bad_request(format!("invalid blocker ID `{blocker_str}`")))?;
        ctx.storage.issues()
            .get_issue(&ctx.repo_id, &blocker_id)
            .await
            .map_err(|_| ApiError::bad_request(format!("blocker issue `{blocker_id}` not found")))?;
        new_blocker_ids.push(blocker_id);
    }

    let issue = ctx.storage.issues()
        .update_issue(&ctx.repo_id, &issue_id, update)
        .await
        .map_err(ApiError::from)?;

    // Create `blocks` links for each new blocker.
    for blocker_id in &new_blocker_ids {
        let _ = ctx.storage.links()
            .create_link(
                &ctx.repo_id,
                blocker_id,
                crate::storage::NewIssueLink {
                    target_id: issue_id,
                    relationship: crate::storage::IssueLinkRelationship::Blocks,
                },
            )
            .await;
    }

    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::IssueUpdated { issue_id, fields_changed })
        .await;

    let linked = linked_workspace_ids(&ctx, issue_id).await;
    let (blocked_by, blocking) = links_for_issue(&ctx, issue_id).await;
    Ok(Json(IssueResponse::from_issue(issue, linked, blocked_by, blocking)))
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues/{id}/close",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Issue ID"),
    ),
    request_body = CloseIssueRequest,
    responses(
        (status = 200, description = "Closed issue", body = IssueResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/issues/:id/close` — close an issue with a resolution.
///
/// Returns 404 if the issue does not exist.
pub(super) async fn close_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<CloseIssueRequest>,
) -> Result<Json<IssueResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    let issue = ctx.storage.issues()
        .close_issue(&ctx.repo_id, &issue_id, &body.resolution)
        .await
        .map_err(ApiError::from)?;

    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::IssueClosed {
            issue_id,
            resolution: body.resolution.clone(),
        })
        .await;

    // Broadcast the close event.
    state.broadcast(BroadcastEvent {
        event_type: "IssueClosed".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({
            "issue_id": issue_id.to_string(),
            "resolution": body.resolution,
        }),
    });

    tracing::info!(
        event = "issue.closed",
        actor = %identity.name,
        repo = %ctx.repo_id,
        issue_id = %issue_id,
        "issue closed"
    );
    let linked = linked_workspace_ids(&ctx, issue_id).await;
    let (blocked_by, blocking) = links_for_issue(&ctx, issue_id).await;
    Ok(Json(IssueResponse::from_issue(issue, linked, blocked_by, blocking)))
}

// ── Issue comment handlers ────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues/{id}/comments",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    request_body = CreateCommentRequest,
    responses(
        (status = 201, description = "Comment created", body = CommentResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/repos/:repo/issues/:id/comments` — add a comment to an issue.
pub(super) async fn create_issue_comment_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<CreateCommentRequest>,
) -> Result<(StatusCode, Json<CommentResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Verify the issue exists.
    ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    // Derive author info from the authenticated identity.
    // For admin-key auth the request body may supply author/author_type/author_id
    // directly (used in tests and management tooling); for API-key and JWT auth
    // the identity is authoritative.
    let (author, author_type, author_id) = match identity.auth_source {
        AuthSource::Jwt => {
            let id = identity.user_id.map(|u| u.to_string())
                .unwrap_or_else(|| identity.key_id.clone());
            (identity.name.clone(), "human".to_string(), Some(id))
        }
        AuthSource::ApiKey => {
            (identity.name.clone(), "agent".to_string(), Some(identity.key_id.clone()))
        }
        AuthSource::AdminKey => {
            let author = body.author.clone().unwrap_or_else(|| "admin".to_string());
            let author_type = body.author_type.clone().unwrap_or_else(|| "human".to_string());
            let author_id = body.author_id.clone();
            (author, author_type, author_id)
        }
    };

    // Parse and validate optional parent_id.
    let parent_id = if let Some(ref pid_str) = body.parent_id {
        let pid = uuid::Uuid::parse_str(pid_str)
            .map_err(|_| ApiError::bad_request(format!("invalid parent_id `{pid_str}`")))?;
        // Verify the parent comment exists on the same issue.
        let existing = ctx.storage.comments()
            .list_comments(&ctx.repo_id, &issue_id)
            .await
            .map_err(ApiError::from)?;
        if !existing.iter().any(|c| c.id == pid) {
            return Err(ApiError::bad_request(
                format!("parent_id `{pid_str}` does not reference a comment on this issue"),
            ));
        }
        Some(pid)
    } else {
        None
    };

    // Resolve @mentions from the body against repo members.
    let mention_names = extract_mention_names(&body.body);
    let new_mentions = resolve_comment_mentions(&ctx.storage, &ctx.repo_id, mention_names).await;

    let comment = ctx.storage.comments()
        .create_comment(&ctx.repo_id, &issue_id, crate::storage::NewIssueComment {
            author: author.clone(),
            body: body.body,
            author_type: author_type.clone(),
            author_id,
            parent_id,
        })
        .await
        .map_err(ApiError::from)?;

    // Store mentions and collect mention UUIDs for the event payload.
    let mentions = ctx.storage.comments()
        .replace_mentions(&ctx.repo_id, &comment.id, new_mentions)
        .await
        .unwrap_or_default();
    let mention_ids: Vec<uuid::Uuid> = mentions.iter().filter_map(|m| m.entity_id()).collect();

    // Append CommentCreated event — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::CommentCreated {
            issue_id,
            comment_id: comment.id,
            author: author.clone(),
            author_type: author_type.clone(),
            parent_id: comment.parent_id,
            mentions: mention_ids.clone(),
        })
        .await;

    state.broadcast(BroadcastEvent {
        event_type: "CommentCreated".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: comment.created_at.to_rfc3339(),
        data: serde_json::json!({
            "issue_id": issue_id.to_string(),
            "comment_id": comment.id.to_string(),
            "author": author,
            "author_type": author_type,
            "parent_id": comment.parent_id.map(|u| u.to_string()),
            "mentions": mention_ids.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
        }),
    });

    Ok((StatusCode::CREATED, Json(CommentResponse::with_mentions(comment, &mentions))))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}/comments",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    responses(
        (status = 200, description = "List of comments", body = Vec<CommentResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/repos/:repo/issues/:id/comments` — list comments for an issue.
pub(super) async fn list_issue_comments_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<Vec<CommentResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Verify the issue exists.
    ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    let comments_store = ctx.storage.comments();
    let (comments, mentions_map) = tokio::join!(
        comments_store.list_comments(&ctx.repo_id, &issue_id),
        comments_store.list_issue_mentions(&ctx.repo_id, &issue_id),
    );
    let comments = comments.map_err(ApiError::from)?;
    let mut mentions_by_comment = mentions_map.unwrap_or_default();

    Ok(Json(
        comments
            .into_iter()
            .map(|c| {
                let id = c.id;
                let mentions = mentions_by_comment.remove(&id).unwrap_or_default();
                CommentResponse::with_mentions(c, &mentions)
            })
            .collect(),
    ))
}

#[utoipa::path(
    patch,
    path = "/api/repos/{repo}/issues/{id}/comments/{comment_id}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("comment_id" = String, Path, description = "Comment UUID"),
    ),
    request_body = UpdateCommentRequest,
    responses(
        (status = 200, description = "Comment updated", body = CommentResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — not the comment author", body = ErrorBody),
        (status = 404, description = "Comment not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `PATCH /api/repos/:repo/issues/:id/comments/:comment_id` — edit a comment body.
///
/// Only the original author may edit a comment.
pub(super) async fn update_issue_comment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
    Json(body): Json<UpdateCommentRequest>,
) -> Result<Json<CommentResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let issue_id_str = params.get("id").cloned().unwrap_or_default();
    let comment_id_str = params.get("comment_id").cloned().unwrap_or_default();

    let issue_id = uuid::Uuid::parse_str(&issue_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{issue_id_str}`")))?;
    let comment_id = uuid::Uuid::parse_str(&comment_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid comment ID `{comment_id_str}`")))?;

    // Verify the issue exists.
    ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    // Find the comment to check ownership.
    let comments = ctx.storage.comments()
        .list_comments(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;
    let comment = comments.into_iter().find(|c| c.id == comment_id)
        .ok_or_else(|| ApiError::not_found(format!("comment `{comment_id_str}` not found")))?;

    if !is_comment_author(&identity, &comment) {
        return Err(ApiError::forbidden("only the original author may edit this comment"));
    }

    // Re-parse and replace @mentions for the new body.
    let mention_names = extract_mention_names(&body.body);
    let new_mentions = resolve_comment_mentions(&ctx.storage, &ctx.repo_id, mention_names).await;

    let updated = ctx.storage.comments()
        .update_comment(&ctx.repo_id, &comment_id, &body.body)
        .await
        .map_err(ApiError::from)?;

    let mentions = ctx.storage.comments()
        .replace_mentions(&ctx.repo_id, &comment_id, new_mentions)
        .await
        .unwrap_or_default();

    Ok(Json(CommentResponse::with_mentions(updated, &mentions)))
}

#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/issues/{id}/comments/{comment_id}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("comment_id" = String, Path, description = "Comment UUID"),
    ),
    responses(
        (status = 200, description = "Comment deleted (soft)", body = CommentResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — not the comment author or admin", body = ErrorBody),
        (status = 404, description = "Comment not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `DELETE /api/repos/:repo/issues/:id/comments/:comment_id` — soft-delete a comment.
///
/// The original author or any admin may delete a comment. The comment is not
/// removed from the database; it is soft-deleted by setting `deleted_at`.
pub(super) async fn delete_issue_comment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<Json<CommentResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let issue_id_str = params.get("id").cloned().unwrap_or_default();
    let comment_id_str = params.get("comment_id").cloned().unwrap_or_default();

    let issue_id = uuid::Uuid::parse_str(&issue_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{issue_id_str}`")))?;
    let comment_id = uuid::Uuid::parse_str(&comment_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid comment ID `{comment_id_str}`")))?;

    // Verify the issue exists.
    ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    // Find the comment to check ownership.
    let comments = ctx.storage.comments()
        .list_comments(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;
    let comment = comments.into_iter().find(|c| c.id == comment_id)
        .ok_or_else(|| ApiError::not_found(format!("comment `{comment_id_str}` not found")))?;

    if !identity.is_admin && !is_comment_author(&identity, &comment) {
        return Err(ApiError::forbidden("only the original author or an admin may delete this comment"));
    }

    let deleted = ctx.storage.comments()
        .soft_delete_comment(&ctx.repo_id, &comment_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(CommentResponse::from(deleted)))
}

// ── Issue link handlers ───────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues/{id}/links",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    request_body = CreateIssueLinkRequest,
    responses(
        (status = 201, description = "Link created", body = IssueLinkResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/repos/:repo/issues/:id/links` — create a link from this issue to another.
pub(super) async fn create_issue_link_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<CreateIssueLinkRequest>,
) -> Result<(StatusCode, Json<IssueLinkResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let source_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;
    let target_id = uuid::Uuid::parse_str(&body.target_id)
        .map_err(|_| ApiError::bad_request(format!("invalid target_id `{}`", body.target_id)))?;

    let relationship = crate::storage::IssueLinkRelationship::from_db_str(&body.relationship)
        .ok_or_else(|| ApiError::bad_request(format!(
            "invalid relationship `{}`, must be one of: blocks, relates-to, duplicates",
            body.relationship
        )))?;

    // Verify both issues exist.
    ctx.storage.issues().get_issue(&ctx.repo_id, &source_id).await.map_err(ApiError::from)?;
    ctx.storage.issues().get_issue(&ctx.repo_id, &target_id).await.map_err(ApiError::from)?;

    ctx.storage.links().create_link(
        &ctx.repo_id,
        &source_id,
        crate::storage::NewIssueLink { target_id, relationship: relationship.clone() },
    ).await.map_err(ApiError::from)?;

    Ok((StatusCode::CREATED, Json(IssueLinkResponse {
        other_issue_id: target_id.to_string(),
        relationship: relationship.as_str().to_string(),
    })))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}/links",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    responses(
        (status = 200, description = "List of links", body = Vec<IssueLinkResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/repos/:repo/issues/:id/links` — list all links for an issue.
pub(super) async fn list_issue_links_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<Vec<IssueLinkResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    ctx.storage.issues().get_issue(&ctx.repo_id, &issue_id).await.map_err(ApiError::from)?;

    let links = ctx.storage.links().list_links(&ctx.repo_id, &issue_id).await.map_err(ApiError::from)?;

    let resp: Vec<IssueLinkResponse> = links.into_iter().map(|link| {
        // Determine direction: if this issue is the source, use the forward relationship;
        // if it's the target, express the inverse from this issue's perspective.
        if link.source_id == issue_id {
            IssueLinkResponse {
                other_issue_id: link.target_id.to_string(),
                relationship: link.relationship.as_str().to_string(),
            }
        } else {
            IssueLinkResponse {
                other_issue_id: link.source_id.to_string(),
                relationship: link.relationship.inverse_str().to_string(),
            }
        }
    }).collect();

    Ok(Json(resp))
}

#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/issues/{id}/links/{target_id}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("target_id" = String, Path, description = "Target issue UUID"),
    ),
    responses(
        (status = 204, description = "Link removed"),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `DELETE /api/repos/:repo/issues/:id/links/:target_id` — remove a link.
pub(super) async fn delete_issue_link_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<StatusCode, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let id = params.get("id").cloned().unwrap_or_default();
    let target_id_str = params.get("target_id").cloned().unwrap_or_default();

    let source_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;
    let target_id_parsed = uuid::Uuid::parse_str(&target_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid target_id `{target_id_str}`")))?;

    ctx.storage.links().delete_link(&ctx.repo_id, &source_id, &target_id_parsed).await.map_err(ApiError::from)?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Attachment handlers ───────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues/{id}/attachments",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    request_body = UploadAttachmentRequest,
    responses(
        (status = 201, description = "Attachment uploaded", body = AttachmentResponse),
        (status = 400, description = "Bad request (invalid filename, size, or count limit)", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
        (status = 409, description = "Attachment with this filename already exists", body = ErrorBody),
        (status = 413, description = "File exceeds 10 MiB limit", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/repos/:repo/issues/:id/attachments` — upload a file attachment.
///
/// Accepts either `multipart/form-data` (fields: `file`, optional `uploaded_by`,
/// `filename`, `content_type`) or a JSON body (`UploadAttachmentRequest`) with
/// the file bytes base64-encoded in the `content` field.
///
/// Limits: 10 MiB per file, 10 attachments per issue.
pub(super) async fn upload_attachment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
    request: axum::extract::Request,
) -> Result<(StatusCode, Json<AttachmentResponse>), ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Verify the issue exists.
    ctx.storage
        .issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    // Enforce per-issue attachment limit.
    let existing = ctx
        .storage
        .attachments()
        .list_attachments(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;
    if existing.len() >= MAX_ATTACHMENTS_PER_ISSUE {
        return Err(ApiError::bad_request(format!(
            "issue already has {MAX_ATTACHMENTS_PER_ISSUE} attachments (limit reached)"
        )));
    }

    let (filename, content_type, bytes, uploaded_by) =
        parse_attachment_body(request).await?;

    validate_attachment_filename(&filename)?;

    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "file exceeds 10 MiB limit ({} bytes)",
            bytes.len()
        )));
    }

    // Store file bytes under a deterministic S3 key.
    let s3_key = format!("issues/{issue_id}/attachments/{filename}");
    ctx.storage
        .files()
        .put(&ctx.repo_id, &s3_key, &bytes)
        .await
        .map_err(|e| ApiError::internal(format!("store attachment: {e}")))?;

    // Persist metadata.
    let attachment = ctx
        .storage
        .attachments()
        .create_attachment(
            &ctx.repo_id,
            &issue_id,
            crate::storage::NewIssueAttachment {
                filename,
                content_type,
                size_bytes: bytes.len() as i64,
                s3_key,
                uploaded_by,
            },
        )
        .await
        .map_err(ApiError::from)?;

    Ok((StatusCode::CREATED, Json(AttachmentResponse::from(attachment))))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}/attachments",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    responses(
        (status = 200, description = "List of attachment metadata", body = Vec<AttachmentResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/repos/:repo/issues/:id/attachments` — list attachment metadata for an issue.
pub(super) async fn list_attachments_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<Vec<AttachmentResponse>>, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Verify the issue exists.
    ctx.storage
        .issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    let attachments = ctx
        .storage
        .attachments()
        .list_attachments(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(
        attachments.into_iter().map(AttachmentResponse::from).collect(),
    ))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}/attachments/{filename}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("filename" = String, Path, description = "Attachment filename"),
    ),
    responses(
        (status = 200, description = "File content (binary)", content_type = "application/octet-stream"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Attachment not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/repos/:repo/issues/:id/attachments/:filename` — download attachment content.
pub(super) async fn download_attachment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await?;

    let id = params.get("id").cloned().unwrap_or_default();
    let filename = params.get("filename").cloned().unwrap_or_default();

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Load metadata to get content_type and s3_key.
    let meta = ctx
        .storage
        .attachments()
        .get_attachment(&ctx.repo_id, &issue_id, &filename)
        .await
        .map_err(ApiError::from)?;

    // Fetch file bytes from the file store.
    let bytes = ctx
        .storage
        .files()
        .get(&ctx.repo_id, &meta.s3_key)
        .await
        .map_err(|e| ApiError::internal(format!("retrieve attachment: {e}")))?;

    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, meta.content_type)
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(axum::body::Body::from(bytes))
        .map_err(|e| ApiError::internal(format!("build response: {e}")))?;

    Ok(response)
}

#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/issues/{id}/attachments/{filename}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("filename" = String, Path, description = "Attachment filename"),
    ),
    responses(
        (status = 204, description = "Attachment deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Attachment not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `DELETE /api/repos/:repo/issues/:id/attachments/:filename` — delete an attachment.
pub(super) async fn delete_attachment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<StatusCode, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;

    let id = params.get("id").cloned().unwrap_or_default();
    let filename = params.get("filename").cloned().unwrap_or_default();

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Load metadata to confirm existence and get s3_key.
    let meta = ctx
        .storage
        .attachments()
        .get_attachment(&ctx.repo_id, &issue_id, &filename)
        .await
        .map_err(ApiError::from)?;

    // Delete file bytes from the file store.
    let _ = ctx
        .storage
        .files()
        .delete(&ctx.repo_id, &meta.s3_key)
        .await;

    // Delete metadata record.
    ctx.storage
        .attachments()
        .delete_attachment(&ctx.repo_id, &issue_id, &filename)
        .await
        .map_err(ApiError::from)?;

    Ok(StatusCode::NO_CONTENT)
}
