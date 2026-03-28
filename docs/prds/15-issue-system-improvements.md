# PRD 15: Issue System Improvements (Server)

## Overview

Complete the issue system to support rich agent-human collaboration. The server needs attachment endpoints, consolidation of dependencies into the link system, and comment schema improvements.

## 1. Issue Attachments API

### Storage
Attachments stored in S3 at `issues/{issue_id}/attachments/{filename}`. Metadata tracked in Postgres `issue_attachments` table.

### Schema
```sql
CREATE TABLE issue_attachments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    issue_id UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    s3_key TEXT NOT NULL,
    uploaded_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (issue_id, filename)
);
```

### Endpoints
- `POST /api/repos/:repo/issues/:id/attachments` — upload file (multipart form data or base64 JSON)
- `GET /api/repos/:repo/issues/:id/attachments` — list attachments (metadata only)
- `GET /api/repos/:repo/issues/:id/attachments/:filename` — download file content
- `DELETE /api/repos/:repo/issues/:id/attachments/:filename` — delete attachment

### Constraints
- Max file size: 10MB per attachment
- Max attachments per issue: 10
- Accepted content types: images, PDFs, text files, JSON, YAML, TOML, CSV (allowlist)
- S3 key format: `{repo_id}/issues/{issue_id}/attachments/{filename}`

### Agent Usage
Agents download attachments for context (screenshots, design docs, error logs). Agents with write permission can upload (audit reports, generated diagrams). Security controls deferred to separate PRD.

### OpenAPI
All endpoints annotated with `#[utoipa::path]`. Request/response types derive `ToSchema`. Attachment metadata included in issue detail response.

### Issues
1. **Add issue_attachments table and storage trait** — Schema migration, `AttachmentStore` trait, Postgres implementation
2. **Implement attachment upload/download/delete endpoints** — Handlers, S3 integration, size/type validation
3. **Include attachments in issue detail response** — List attachments when fetching issue detail

## 2. Consolidate depends_on into Issue Links

### Problem
Two overlapping systems: `depends_on` field on issues AND `blocks` relationship in issue links. This is confusing and redundant.

### Solution
Remove `depends_on` from the issues table. The `blocks` link type becomes the single mechanism for both informational blocking and work queue ordering.

### Work Queue Changes
Current: work queue checks `issue.depends_on` array for unresolved dependencies.
New: work queue checks `issue_links` table for `blocks` relationships where the blocking issue is still open.

```sql
-- Issue B is blocked if any issue A exists where:
-- A has a "blocks" link targeting B, AND A.status != 'closed'
SELECT DISTINCT il.target_id AS blocked_issue_id
FROM issue_links il
JOIN issues i ON i.id = il.source_id AND i.repo_id = il.repo_id
WHERE il.relationship = 'blocks'
AND i.status != 'closed';
```

### Migration
1. For each issue with `depends_on` entries, create `blocks` links (source=dependency, target=issue)
2. Drop `depends_on` column and `issue_dependencies` table
3. Update CreateIssueRequest: replace `depends_on: Vec<String>` with `blocked_by: Vec<String>` which creates `blocks` links
4. Update work queue to query links instead of depends_on

### Issues
4. **Migrate depends_on to blocks links** — Data migration, schema change, update CreateIssueRequest
5. **Update work queue to use issue links for blocking** — Replace depends_on check with link query
6. **Add blocked_by convenience field to CreateIssueRequest** — Accept issue IDs that should block this issue, auto-create links

## 3. Comment Schema Improvements

### Changes
Add fields to support future agent interaction:
- `author_type TEXT NOT NULL DEFAULT 'human'` — either `'human'` or `'agent'`
- `author_id TEXT` — user ID or agent identifier
- `body` supports markdown (no schema change needed — stored as plain text, rendered by client)

### Migration
```sql
ALTER TABLE issue_comments ADD COLUMN author_type TEXT NOT NULL DEFAULT 'human';
ALTER TABLE issue_comments ADD COLUMN author_id TEXT;
```

### Issues
7. **Add author_type and author_id to issue comments** — Schema migration, update create comment handler, include in response

## 4. Issue Detail Response Enhancement

The issue detail endpoint should return a complete picture:
- Issue metadata (title, description, status, priority, labels, creator, acceptance_criteria)
- Linked issues with relationship types and status (from issue_links)
- Attachments list (from issue_attachments)
- Comments with author info (from issue_comments)
- Linked workspaces (already present)

### Issues
8. **Enrich issue detail response with links, attachments, and comments** — Single endpoint returns everything needed for the detail page

## Issue Summary

| # | Issue | Priority | Depends On |
|---|-------|----------|------------|
| 1 | Add issue_attachments table and storage trait | High | — |
| 2 | Implement attachment upload/download/delete endpoints | High | 1 |
| 3 | Include attachments in issue detail response | Medium | 2 |
| 4 | Migrate depends_on to blocks links | High | — |
| 5 | Update work queue to use issue links for blocking | High | 4 |
| 6 | Add blocked_by convenience field to CreateIssueRequest | Medium | 4 |
| 7 | Add author_type and author_id to issue comments | Medium | — |
| 8 | Enrich issue detail response with links, attachments, comments | Medium | 2, 7 |

## Deferred

- **Issue templates** — future PRD, likely `.vai/issue-templates/` in the repo
- **Agent mention/tagging in comments** — future PRD, ties into watcher and agent registry
- **Contract-based parallel execution** — future PRD, implementation plans as contracts that unblock dependent issues
- **Attachment security controls** — separate security PRD (rate limits, type allowlists, virus scanning)
