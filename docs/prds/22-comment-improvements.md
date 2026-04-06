# PRD: Issue Comment Improvements

## Status

Proposed

## Context

Issue comments are partially implemented but broken in production — the dashboard sends `{ body }` but the server requires `{ author, body }`, so posting comments fails. Beyond that fix, the comment system lacks threading, editing, deletion, @mentions, and real-time notifications.

This PRD fixes the immediate bug and builds out a full comment system suitable for human-agent collaboration.

## Design Decisions

### Author Derivation

The server derives the comment author from the authenticated identity — the client never sends author info.

- **JWT auth (humans):** `author` = `identity.name`, `author_id` = `identity.user_id`, `author_type` = "human"
- **API key auth (agents):** `author` = `identity.name` (key name), `author_id` = `identity.key_id`, `author_type` = "agent"
- **Admin key:** `author` = "admin", `author_type` = "human"

The `author` field becomes optional in `CreateCommentRequest`. If provided, it's ignored (reserved for future admin override).

### Threading Model

Single-level threading (GitHub-style):
- Comments have an optional `parent_id` (UUID, nullable FK to comments table)
- Top-level comments: `parent_id` is null
- Replies: `parent_id` points to a top-level comment
- No deeper nesting — replying to a reply attaches to the same parent
- UI groups replies under their parent with collapse/expand

### @Mentions

Server-side parsing. When a comment is created or edited, the server:
1. Scans the body for `@word` patterns
2. Validates each against known users and API keys with repo access
3. Stores valid mentions in a `comment_mentions` join table
4. Includes mention data in the `CommentCreated` event

Dashboard provides autocomplete via a members search endpoint:
```
GET /api/repos/:repo/members?q=search_term
```
Returns up to 10 matching users and agents with repo access.

### Notifications

`CommentCreated` event added to the event stream, including mentioned user IDs. The dashboard:
- Shows a toast when the current user is mentioned in a new comment
- Displays a notification bell with unread mention count
- External notifications (email, webhooks) deferred to future PRD

### Edit & Delete

- **Edit:** Original author only. Stores `edited_at` timestamp. UI shows "(edited)" indicator.
- **Delete:** Original author or admins. Soft delete via `deleted_at` timestamp. Renders as "This comment was deleted."
- No edit history.

## Schema Changes

### Migration: comment threading + edit/delete

```sql
ALTER TABLE issue_comments ADD COLUMN parent_id UUID REFERENCES issue_comments(id) ON DELETE SET NULL;
ALTER TABLE issue_comments ADD COLUMN edited_at TIMESTAMPTZ;
ALTER TABLE issue_comments ADD COLUMN deleted_at TIMESTAMPTZ;
CREATE INDEX idx_issue_comments_parent_id ON issue_comments(parent_id);
```

### Migration: comment mentions

```sql
CREATE TABLE comment_mentions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    comment_id UUID NOT NULL REFERENCES issue_comments(id) ON DELETE CASCADE,
    repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    mentioned_user_id UUID,
    mentioned_key_id UUID,
    mentioned_name TEXT NOT NULL,
    mention_type TEXT NOT NULL DEFAULT 'human',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_comment_mentions_user ON comment_mentions(mentioned_user_id);
CREATE INDEX idx_comment_mentions_comment ON comment_mentions(comment_id);
```

## API Changes

### Modified: `POST /api/repos/:repo/issues/:id/comments`

Request body (simplified):
```json
{
  "body": "Looks good! @jordy can you review?",
  "parent_id": null
}
```

`author`, `author_type`, `author_id` are derived server-side from the authenticated identity. `parent_id` is optional (null = top-level comment).

Response includes new fields:
```json
{
  "id": "uuid",
  "issue_id": "uuid",
  "author": "jordy",
  "author_type": "human",
  "author_id": "user-uuid",
  "body": "Looks good! @jordy can you review?",
  "parent_id": null,
  "edited_at": null,
  "deleted_at": null,
  "mentions": [{ "name": "jordy", "type": "human", "id": "user-uuid" }],
  "created_at": "2026-04-04T..."
}
```

### New: `PATCH /api/repos/:repo/issues/:id/comments/:comment_id`

Edit a comment. Only the original author can edit.

```json
{ "body": "Updated comment text" }
```

Server re-parses mentions on edit. Sets `edited_at` timestamp.

### New: `DELETE /api/repos/:repo/issues/:id/comments/:comment_id`

Soft delete. Original author or admins only. Sets `deleted_at` timestamp.

### New: `GET /api/repos/:repo/members?q=search`

Search users and agents with access to the repo. For @mention autocomplete.

```json
[
  { "id": "uuid", "name": "jordy", "type": "human" },
  { "id": "key-uuid", "name": "ralph-worker-1", "type": "agent" }
]
```

Searches users (by name/email) and API keys (by name) scoped to the repo. Max 10 results.

### New Event: `CommentCreated`

Added to `EventKind` enum. Broadcast via Postgres NOTIFY + WebSocket.

```json
{
  "event_type": "CommentCreated",
  "issue_id": "uuid",
  "comment_id": "uuid",
  "author": "jordy",
  "author_type": "human",
  "parent_id": null,
  "mentions": ["user-uuid-1"]
}
```

## Issue Breakdown

### Issue 1: Server — derive author from auth identity, make body-only request work

**Priority:** critical
**Blocks:** All other issues

Fix the immediate bug. Make `author` optional in `CreateCommentRequest`. Derive author, author_type, and author_id from `AgentIdentity` in the handler.

**Files:**
- `src/server/mod.rs` — update `CreateCommentRequest`, update `create_issue_comment_handler`

**Acceptance criteria:**
- `POST /api/repos/:repo/issues/:id/comments` with `{ "body": "text" }` succeeds
- Author is correctly derived from JWT (human name) or API key (key name)
- Existing comment creation with explicit author still works (field ignored, server overrides)
- `cargo test --features full` passes

---

### Issue 2: Server — add parent_id, edited_at, deleted_at to comments schema

**Priority:** high
**Depends on:** Issue 1

Add threading and edit/delete columns.

**Files:**
- New migration file
- `src/issue/mod.rs` — update `IssueComment` and `NewIssueComment` structs
- `src/storage/mod.rs` — update `CommentStore` trait
- `src/storage/postgres.rs` — update queries
- `src/storage/sqlite.rs` — update queries
- `src/server/mod.rs` — update `CreateCommentRequest` to accept `parent_id`, update `CommentResponse`

**Acceptance criteria:**
- Comments can be created with `parent_id` pointing to an existing comment
- `parent_id` validates that the referenced comment exists and belongs to the same issue
- `edited_at` and `deleted_at` returned in responses (null when not set)
- `cargo test --features full` passes

---

### Issue 3: Server — edit and delete comment endpoints

**Priority:** high
**Depends on:** Issue 2

Add PATCH and DELETE endpoints for comments.

**Files:**
- `src/server/mod.rs` — new `edit_comment_handler`, `delete_comment_handler`
- `src/storage/mod.rs` — add `update_comment`, `soft_delete_comment` to trait
- `src/storage/postgres.rs` — implement
- `src/storage/sqlite.rs` — implement

**Endpoints:**
- `PATCH /api/repos/:repo/issues/:id/comments/:comment_id` — edit body, set edited_at
- `DELETE /api/repos/:repo/issues/:id/comments/:comment_id` — set deleted_at

**Acceptance criteria:**
- Only original author can edit (compare identity against comment author_id)
- Only original author or admin can delete
- Edit sets `edited_at` to now
- Delete sets `deleted_at` to now (soft delete)
- Deleted comments return `{ "body": null, "deleted_at": "..." }` in list responses
- 403 if unauthorized
- `cargo test --features full` passes

---

### Issue 4: Server — CommentCreated event, mention parsing, comment_mentions table

**Priority:** high
**Depends on:** Issue 2

Add mention extraction and real-time events for comments.

**Files:**
- New migration for `comment_mentions` table
- `src/event_log/mod.rs` — add `CommentCreated` to `EventKind`
- `src/server/mod.rs` — extract mentions on create/edit, store in `comment_mentions`, broadcast event
- `src/storage/mod.rs` — add mention storage methods
- `src/storage/postgres.rs` — implement

**Mention parsing:**
- Regex scan for `@(\w[\w.-]*)` in comment body
- Validate each match against users (by name) and API keys (by name) with repo access
- Store valid mentions in `comment_mentions`
- On edit: delete old mentions, re-parse and store new ones

**Acceptance criteria:**
- Creating a comment with `@username` stores a mention record
- `CommentCreated` event includes `mentions` array with user IDs
- Event is broadcast via Postgres NOTIFY
- Invalid @mentions (no matching user) are silently ignored
- `cargo test --features full` passes

---

### Issue 5: Server — members search endpoint for @mention autocomplete

**Priority:** medium
**Depends on:** Issue 1

Add a search endpoint for repo members (humans + agents).

**Files:**
- `src/server/mod.rs` — new `search_members_handler`
- Route: `GET /api/repos/:repo/members?q=search_term`

**Searches:**
- Users with repo access (via collaborators or org membership) — match name or email against `q`
- API keys scoped to the repo — match key name against `q`
- Case-insensitive prefix match
- Limit 10 results
- Returns `[{ id, name, type }]`

**Acceptance criteria:**
- `GET /api/repos/:repo/members?q=jor` returns matching users
- `GET /api/repos/:repo/members?q=ralph` returns matching agent keys
- Results limited to 10
- Empty `q` returns first 10 members
- `cargo test --features full` passes

---

### Issue 6: Dashboard — threaded comment UI with reply

**Priority:** high
**Depends on:** Issue 2

Update IssueActivityTimeline to support threaded comments.

**Files:**
- `src/components/issues/IssueActivityTimeline.tsx` — thread grouping, reply button, reply form
- `src/hooks/use-vai.ts` — pass `parent_id` in addComment mutation

**UI behavior:**
- Top-level comments render chronologically
- Replies grouped under parent with indent
- "N replies" collapse/expand toggle (collapsed by default if >2 replies)
- "Reply" button on each comment opens inline reply form
- Reply form sends `parent_id` to server

**Acceptance criteria:**
- Can reply to a comment
- Replies appear grouped under parent
- Thread collapse/expand works
- `pnpm test` passes
- `pnpm test:e2e` passes

---

### Issue 7: Dashboard — edit and delete comment UI

**Priority:** high
**Depends on:** Issue 3

Add edit/delete actions to comments.

**Files:**
- `src/components/issues/IssueActivityTimeline.tsx` — edit/delete buttons, inline edit form
- `src/hooks/use-vai.ts` — add `useEditComment` and `useDeleteComment` mutations

**UI behavior:**
- Edit/delete buttons shown only on comments authored by the current user (or admin for delete)
- Edit: replaces comment body with textarea, Save/Cancel buttons
- Delete: confirmation prompt, then shows "This comment was deleted" placeholder
- "(edited)" indicator next to timestamp for edited comments

**Acceptance criteria:**
- Can edit own comments
- Can delete own comments (or admin can delete any)
- Edited comments show "(edited)"
- Deleted comments show placeholder
- `pnpm test` passes

---

### Issue 8: Dashboard — @mention autocomplete in comment textarea

**Priority:** medium
**Depends on:** Issue 5

Add @mention autocomplete dropdown to the comment textarea.

**Files:**
- New: `src/components/issues/MentionAutocomplete.tsx`
- `src/components/issues/IssueActivityTimeline.tsx` — integrate autocomplete
- `src/hooks/use-vai.ts` — add `useSearchMembers` hook

**UI behavior:**
- Typing `@` in the comment textarea triggers autocomplete dropdown
- Dropdown shows matching users/agents from the members search endpoint
- Selecting a suggestion inserts `@username` into the textarea
- Debounced search (300ms)
- Keyboard navigation (arrow keys, enter to select, escape to dismiss)
- Mentions render as highlighted text in preview tab

**Acceptance criteria:**
- `@` triggers autocomplete with matching results
- Selecting inserts the mention
- Keyboard navigation works
- `pnpm test` passes

---

### Issue 9: Dashboard — notification bell and mention toasts

**Priority:** medium
**Depends on:** Issue 4

Add real-time mention notifications.

**Files:**
- New: `src/components/shared/NotificationBell.tsx`
- `src/hooks/use-events.ts` or new `src/hooks/use-notifications.ts` — listen for CommentCreated events mentioning current user
- `src/components/AppShell.tsx` or `src/components/Sidebar.tsx` — render notification bell

**UI behavior:**
- Bell icon in sidebar/header with unread count badge
- Toast notification when mentioned in a new comment ("@jordy mentioned you in Issue #42")
- Clicking toast navigates to the issue
- Clicking bell shows dropdown with recent mentions
- Mentions marked as read when viewed

**Acceptance criteria:**
- Toast appears when current user is mentioned
- Bell shows unread count
- Clicking navigates to the issue
- `pnpm test` passes
