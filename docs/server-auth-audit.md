# Server Auth Audit

Endpoint-by-endpoint access control audit for the vai server.
Last updated: 2026-04-20 (issue #294).

## Legend

| Access Level | Meaning |
|---|---|
| **Public** | No authentication required. |
| **Authenticated** | Any valid API key, JWT, or admin key. |
| **User-scoped** | Authenticated, and results filtered to the caller's own data. |
| **Repo-collaborator** | `require_repo_permission` — caller must have a collaborator row for the repo. |
| **Admin** | Bootstrap admin key only (`is_admin == true`). |

---

## Public endpoints

These are intentionally public with no auth check.

| Method | Path | Handler | Rationale | Test |
|---|---|---|---|---|
| GET | `/health` | `health_handler` | Basic liveness probe, no data returned | `server::tests::health_ok` |
| GET | `/api/status` | `status_handler` | Read-only server status | — |
| GET | `/api/server/stats` | `server_stats_handler` | Aggregate counters, no user data | — |
| GET | `/api/openapi.json` | `openapi_handler` | API schema, no user data | — |
| POST | `/api/auth/token` | `auth::token_exchange_handler` | Token exchange — must be pre-auth | — |
| POST | `/api/auth/refresh` | `auth::refresh_token_handler` | Refresh token — pre-auth | — |
| POST | `/api/auth/revoke` | `auth::revoke_token_handler` | Token revocation — pre-auth | — |
| POST | `/api/auth/cli-device` | `auth::create_device_code_handler` | Device code initiation — pre-auth | — |
| GET | `/api/auth/cli-device/:code` | `auth::poll_device_code_handler` | Device code polling — pre-auth | — |

---

## Authenticated-only endpoints (all authenticated users)

All routes below the public block require `Authorization: Bearer <key>` (REST) or `?key=<key>` (WS).

| Method | Path | Handler | Access | Filter | Test |
|---|---|---|---|---|---|
| POST | `/api/repos` | `admin::create_repo_handler` | Authenticated | Creator auto-added as owner-collaborator | `test_user_can_create_repo_and_becomes_collaborator` |
| GET | `/api/repos` | `admin::list_repos_handler` | User-scoped | Returns only repos where caller is a collaborator | `test_list_repos_filters_by_collaborator` |
| POST | `/api/users` | `admin::create_user_handler` | Admin | Admin only | — |
| GET | `/api/users/:user` | `admin::get_user_handler` | Admin | Admin only | — |
| POST | `/api/orgs` | `admin::create_org_handler` | Admin | Admin only | — |
| GET | `/api/orgs` | `admin::list_orgs_handler` | Admin | Admin only | — |
| GET | `/api/orgs/:org` | `admin::get_org_handler` | Admin | Admin only | — |
| DELETE | `/api/orgs/:org` | `admin::delete_org_handler` | Admin | Admin only | — |
| POST | `/api/orgs/:org/members` | `admin::add_org_member_handler` | Admin | Admin only | — |
| GET | `/api/orgs/:org/members` | `admin::list_org_members_handler` | Admin | Admin only | — |
| PATCH | `/api/orgs/:org/members/:user` | `admin::update_org_member_handler` | Admin | Admin only | — |
| DELETE | `/api/orgs/:org/members/:user` | `admin::remove_org_member_handler` | Admin | Admin only | — |
| POST | `/api/orgs/:org/repos/:repo/collaborators` | `admin::add_collaborator_handler` | Admin | Admin only | — |
| GET | `/api/orgs/:org/repos/:repo/collaborators` | `admin::list_collaborators_handler` | Admin | Admin only | — |
| PATCH | `/api/orgs/:org/repos/:repo/collaborators/:user` | `admin::update_collaborator_handler` | Admin | Admin only | — |
| DELETE | `/api/orgs/:org/repos/:repo/collaborators/:user` | `admin::remove_collaborator_handler` | Admin | Admin only | — |
| POST | `/api/keys` | `admin::create_key_handler` | User-scoped | Non-admin: creates key for own `user_id` only | `server::tests::api_key_authentication` |
| GET | `/api/keys` | `admin::list_keys_handler` | User-scoped | Non-admin sees only own keys; admin sees all | — |
| DELETE | `/api/keys` | `admin::bulk_revoke_keys_handler` | Admin | Admin only | — |
| DELETE | `/api/keys/:id` | `admin::revoke_key_handler` | User-scoped | Non-admin: can only revoke own keys | — |
| POST | `/api/auth/cli-device/authorize` | `auth::authorize_device_code_handler` | Authenticated | — | `test_device_code_flow` |

---

## Repo-collaborator endpoints (all prefixed `/api/repos/:repo/`)

All endpoints in this group use `require_repo_permission` (Read or Write as noted).
Non-collaborators receive **403 Forbidden**.

### Repo context

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| GET | `/me` | `admin::get_me_handler` | Read | — |
| GET | `/status` | `status_handler` | Read (via `RepoCtx`) | — |

### Workspaces

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| POST | `/workspaces` | `workspace::create_workspace_handler` | Write | `test_repo_access_returns_403_for_non_collaborator` |
| GET | `/workspaces` | `workspace::list_workspaces_handler` | Read | `test_repo_access_returns_403_for_non_collaborator` |
| GET | `/workspaces/:id` | `workspace::get_workspace_handler` | Read | — |
| POST | `/workspaces/:id/submit` | `workspace::submit_workspace_handler` | Write | — |
| POST | `/workspaces/:id/files` | `workspace::upload_workspace_files_handler` | Write | — |
| POST | `/workspaces/:id/upload-snapshot` | `workspace::upload_snapshot_handler` | Write | — |
| GET | `/workspaces/:id/files/*path` | `workspace::get_workspace_file_handler` | Read | — |
| DELETE | `/workspaces/:id` | `workspace::discard_workspace_handler` | Write | — |

### Files

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| GET | `/files` | `list_repo_files_handler` | Read | — |
| POST | `/files` | `upload_source_files_handler` | Write | — |
| GET | `/files/download` | `files_download_handler` | Read | — |
| GET | `/files/pull` | `files_pull_handler` | Read | — |
| GET | `/files/manifest` | `files_manifest_handler` | Read | — |
| GET | `/files/*path` | `get_main_file_handler` | Read | — |

### Versions

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| GET | `/versions` | `version::list_versions_handler` | Read | `test_repo_access_returns_403_for_non_collaborator` |
| POST | `/versions/rollback` | `version::rollback_handler` | Write | — |
| GET | `/versions/:id/diff` | `version::get_version_diff_handler` | Read | — |
| GET | `/versions/:id` | `version::get_version_handler` | Read | — |

### Graph

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| GET | `/graph/entities` | `graph::list_graph_entities_handler` | Read | — |
| GET | `/graph/blast-radius` | `graph::get_blast_radius_handler` | Read | — |
| GET | `/graph/entities/:id` | `graph::get_graph_entity_handler` | Read | — |
| GET | `/graph/entities/:id/deps` | `graph::get_entity_deps_handler` | Read | — |
| POST | `/graph/refresh` | `graph::server_graph_refresh_handler` | Write | — |

### Issues

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| POST | `/issues` | `issue::create_issue_handler` | Write | — |
| GET | `/issues` | `issue::list_issues_handler` | Read | `test_repo_access_returns_403_for_non_collaborator` |
| POST | `/issues/:id/close` | `issue::close_issue_handler` | Write | — |
| POST | `/issues/:id/comments` | `issue::create_issue_comment_handler` | Write | — |
| GET | `/issues/:id/comments` | `issue::list_issue_comments_handler` | Read | — |
| PATCH | `/issues/:id/comments/:comment_id` | `issue::update_issue_comment_handler` | Write | — |
| DELETE | `/issues/:id/comments/:comment_id` | `issue::delete_issue_comment_handler` | Write | — |
| POST | `/issues/:id/links` | `issue::create_issue_link_handler` | Write | — |
| GET | `/issues/:id/links` | `issue::list_issue_links_handler` | Read | — |
| DELETE | `/issues/:id/links/:target_id` | `issue::delete_issue_link_handler` | Write | — |
| POST | `/issues/:id/attachments` | `issue::upload_attachment_handler` | Write | — |
| GET | `/issues/:id/attachments` | `issue::list_attachments_handler` | Read | — |
| GET | `/issues/:id/attachments/:filename` | `issue::download_attachment_handler` | Read | — |
| DELETE | `/issues/:id/attachments/:filename` | `issue::delete_attachment_handler` | Write | — |
| GET | `/issues/:id` | `issue::get_issue_handler` | Read | — |
| PATCH | `/issues/:id` | `issue::update_issue_handler` | Write | — |

### Escalations

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| GET | `/escalations` | `escalation::list_escalations_handler` | Read | — |
| POST | `/escalations/:id/resolve` | `escalation::resolve_escalation_handler` | Write | — |
| GET | `/escalations/:id` | `escalation::get_escalation_handler` | Read | — |

### Work queue

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| GET | `/work-queue` | `work_queue::get_work_queue_handler` | Read | — |
| POST | `/work-queue/claim` | `work_queue::claim_work_handler` | Write | — |

### Watchers

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| POST | `/watchers/register` | `watcher::register_watcher_handler` | Write | — |
| GET | `/watchers` | `watcher::list_watchers_handler` | Read | — |
| POST | `/watchers/:id/pause` | `watcher::pause_watcher_handler` | Write | — |
| POST | `/watchers/:id/resume` | `watcher::resume_watcher_handler` | Write | — |
| POST | `/discoveries` | `watcher::submit_discovery_handler` | Write | — |

### Members

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| GET | `/members` | `admin::search_repo_members_handler` | Read | — |

### WebSocket

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| GET (WS) | `/ws/events` | `ws::ws_events_handler` | Read | `test_ws_events_rejects_non_collaborator` |

### Migration

| Method | Path | Handler | Required Role | Test |
|---|---|---|---|---|
| POST | `/migrate` | `migrate_handler` | Write (admin in practice) | — |
| GET | `/migration-stats` | `migration_stats_handler` | Read | — |

---

## Implementation notes

- `require_repo_permission` is a no-op in SQLite (local) mode — any authenticated key gets `Owner`.
  In Postgres (server) mode it queries `repo_collaborators` and returns 403 if no row is found.
- The bootstrap admin key (`VAI_ADMIN_KEY`) bypasses all per-repo checks and always returns `Owner`.
- WebSocket connections authenticate via the `?key=<token>` query parameter (not `Authorization:`)
  because the HTTP upgrade handshake does not support custom headers in most browser implementations.
  As of issue #294 the WS handler now also calls `require_repo_permission` after authentication,
  preventing cross-tenant event leakage in Postgres server mode.
