# PRD: Server-Side Pagination for List Endpoints

## Status

Proposed

## Context

All list endpoints currently return complete arrays with no pagination. The vai-dashboard has 150+ issues and growing — tables render the entire dataset on every load. With 50-500 agents working on a system, these lists will grow exponentially.

This PRD adds server-side pagination to all dashboard-facing list endpoints with a consistent response envelope, then updates the dashboard to use URL-persisted pagination state via nuqs + TanStack Table.

## Design Decisions

### Pagination Style

Offset/limit (page-based) for now. The response envelope is designed to accommodate cursor-based pagination later without breaking the frontend contract.

### Response Envelope

All paginated endpoints return:

```json
{
  "data": [...],
  "pagination": {
    "page": 2,
    "per_page": 25,
    "total": 342,
    "total_pages": 14
  }
}
```

Future cursor fields (`next_cursor`, `prev_cursor`) can be added without breaking existing consumers.

### Query Parameters

All paginated endpoints accept:

```
?page=1&per_page=25&sort=created_at:desc,priority:asc
```

- `page` — 1-indexed, default 1
- `per_page` — default 25, max 100, min 1
- `sort` — comma-separated `column:direction` pairs, e.g. `created_at:desc,priority:asc`

Each endpoint defines an allowlist of sortable columns. Unknown columns return 400.

Existing filters (e.g. `status`, `priority` on issues) continue to work alongside pagination. `total` reflects the filtered count.

### Defaults

- Default `per_page`: 25
- Maximum `per_page`: 100
- Default sort: `created_at:desc` (most recent first)

### Endpoints Affected

**Paginated (dashboard-facing):**
- `GET /api/repos/:repo/issues`
- `GET /api/repos/:repo/workspaces`
- `GET /api/repos/:repo/versions`
- `GET /api/repos/:repo/escalations`

**NOT paginated (agent-facing):**
- `GET /api/repos/:repo/work-queue` — agents need full ranked list for claiming
- `GET /api/repos/:repo/graph/entities` — defer to future PRD

### Shared Infrastructure (no new dependencies)

```rust
// src/storage/pagination.rs

pub struct ListQuery {
    pub page: u32,        // 1-indexed
    pub per_page: u32,    // default 25, max 100
    pub sort: Vec<SortField>,
}

pub struct SortField {
    pub column: String,
    pub direction: SortDirection,
}

pub enum SortDirection {
    Asc,
    Desc,
}

pub struct ListResult<T> {
    pub items: Vec<T>,
    pub total: u64,
}
```

Server-side:

```rust
// src/server/pagination.rs

#[derive(Deserialize, ToSchema)]
pub struct PaginationParams {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub sort: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub pagination: PaginationMeta,
}

#[derive(Serialize, ToSchema)]
pub struct PaginationMeta {
    pub page: u32,
    pub per_page: u32,
    pub total: u64,
    pub total_pages: u32,
}
```

### Storage Layer Changes

All list trait methods change signature:

```rust
// Before
async fn list_issues(&self, repo_id: &Uuid, filter: &IssueFilter) -> Result<Vec<Issue>>;

// After
async fn list_issues(&self, repo_id: &Uuid, filter: &IssueFilter, query: &ListQuery) -> Result<ListResult<Issue>>;
```

SQL implementations use `LIMIT`, `OFFSET`, `ORDER BY`, and a `SELECT COUNT(*)` with the same `WHERE` clause for the total.

### Sortable Columns Per Endpoint

| Endpoint | Sortable Columns |
|----------|-----------------|
| Issues | `created_at`, `updated_at`, `priority`, `status`, `title` |
| Workspaces | `created_at`, `updated_at`, `status`, `intent` |
| Versions | `created_at`, `version_id` |
| Escalations | `created_at`, `status` |

### Dashboard Changes

- Install `nuqs` for URL-persisted query state
- Update TanStack Query hooks to pass pagination/sort/filter params
- Configure TanStack Table with `manualPagination: true`, `manualSorting: true`
- Serialize table state to URL via nuqs: `?page=2&per_page=25&sort=created_at:desc`
- Update orval config or hand-written hooks to handle the new response envelope
- Update e2e tests if list page assertions change

## Issue Breakdown

### Issue 1: Shared pagination module — types, parsing, SQL helpers

**Priority:** high
**Blocks:** Issues 2-5

Create the shared pagination infrastructure in the vai server.

**Files:**
- `src/storage/pagination.rs` — `ListQuery`, `SortField`, `SortDirection`, `ListResult<T>`, parsing/validation helpers
- `src/storage/mod.rs` — re-export pagination types
- `src/server/pagination.rs` — `PaginationParams`, `PaginatedResponse<T>`, `PaginationMeta`, axum extractor helpers
- `src/server/mod.rs` — re-export server pagination types

**Tasks:**
- `ListQuery::from_params(page, per_page, sort_str, allowed_columns)` — parses and validates, returns 400-friendly errors
- `ListQuery::sql_order_by(&self, column_map: &HashMap<&str, &str>)` — generates `ORDER BY` clause with column name mapping (API name → DB column)
- `ListQuery::sql_limit_offset(&self)` — generates `LIMIT ? OFFSET ?` values
- `PaginatedResponse::new(items, total, query)` — constructs response with computed `total_pages`
- Unit tests for parsing, validation, edge cases (page 0, negative per_page, unknown sort column, empty sort)

**Acceptance criteria:**
- `cargo test` passes
- `cargo clippy` clean
- Sort string parsing handles: single column, multi-column, missing direction (defaults to asc), invalid format

---

### Issue 2: Update storage traits and SQLite implementation for pagination

**Priority:** high
**Depends on:** Issue 1
**Blocks:** Issues 3-5

Update the storage trait methods and SQLite implementation to accept `ListQuery` and return `ListResult<T>`.

**Files:**
- `src/storage/mod.rs` — update trait signatures for `list_issues`, `list_workspaces`, `list_versions`, `list_escalations`
- `src/storage/sqlite.rs` — update SQLite implementations with `ORDER BY`, `LIMIT`, `OFFSET`, `COUNT(*)`

**Tasks:**
- Update 4 trait methods to accept `&ListQuery` and return `Result<ListResult<T>>`
- Update SQLite implementations to build dynamic SQL with sort/pagination
- Handle the `COUNT(*)` query efficiently (same WHERE, no ORDER BY/LIMIT)
- Update all call sites that use these methods (CLI commands, server handlers, tests)
- Ensure backward compatibility: call sites that don't need pagination can pass `ListQuery::default()` which returns all results (page 1, per_page u32::MAX)

**Acceptance criteria:**
- `cargo test` passes (CLI-only build)
- All existing functionality works unchanged
- `ListQuery::default()` returns unpaginated results for backward compatibility

---

### Issue 3: Update Postgres storage implementation for pagination

**Priority:** high
**Depends on:** Issue 2

Update the Postgres storage implementation to match the new trait signatures.

**Files:**
- `src/storage/postgres.rs` — update all `list_*` implementations with `ORDER BY`, `LIMIT`, `OFFSET`, `COUNT(*)`

**Tasks:**
- Mirror the SQLite pagination changes for Postgres
- Use parameterized queries for LIMIT/OFFSET (not string interpolation)
- Use `SELECT COUNT(*)` with same WHERE clause for totals
- Run against Postgres in tests

**Acceptance criteria:**
- `cargo test --features full` passes
- Postgres queries use proper parameterized pagination

---

### Issue 4: Update list endpoint handlers and OpenAPI annotations

**Priority:** high
**Depends on:** Issues 2, 3

Update the 4 list handlers to accept pagination params and return `PaginatedResponse<T>`.

**Files:**
- `src/server/mod.rs` — update `list_issues_handler`, `list_workspaces_handler`, `list_versions_handler`, `list_escalations_handler`
- OpenAPI `#[utoipa::path]` annotations on all 4 handlers

**Tasks:**
- Add `PaginationParams` to each handler's query extraction (merge with existing filter params where applicable)
- Parse and validate sort columns against per-endpoint allowlists
- Call storage with `ListQuery`, wrap result in `PaginatedResponse`
- Update OpenAPI annotations to reflect new response schema
- Update existing server integration tests
- Remove the old `MAX_PAGE_SIZE` / `ListVersionsQuery.limit` logic (superseded by shared pagination)

**Sortable columns:**
- Issues: `created_at`, `updated_at`, `priority`, `status`, `title`
- Workspaces: `created_at`, `updated_at`, `status`, `intent`
- Versions: `created_at`, `version_id`
- Escalations: `created_at`, `status`

**Acceptance criteria:**
- All 4 endpoints return `{data: [...], pagination: {...}}` format
- `?page=1&per_page=10` works on all endpoints
- `?sort=created_at:desc` works on all endpoints
- Existing filters still work alongside pagination
- `GET /api/openapi.json` reflects the new response shapes
- `cargo test --features full` passes

---

### Issue 5: Dashboard — install nuqs, update hooks and tables for server-side pagination

**Priority:** high
**Depends on:** Issue 4

Update the vai-dashboard to consume paginated responses with URL-persisted state.

**Files:**
- `package.json` — add `nuqs` dependency
- `src/hooks/use-vai.ts` — update list hooks to accept and pass pagination/sort params
- `src/components/issues/IssueTable.tsx` — server-side pagination + sorting via nuqs
- `src/components/workspaces/WorkspaceTable.tsx` — same
- `src/components/versions/VersionTable.tsx` — same
- `src/components/escalations/EscalationTable.tsx` — same
- `src/routes/$repoSlug/issues/index.tsx` — wire up nuqs query params
- `src/routes/$repoSlug/workspaces.tsx` — wire up nuqs query params
- `src/routes/$repoSlug/versions/index.tsx` — wire up nuqs query params
- `src/routes/$repoSlug/escalations/index.tsx` — wire up nuqs query params
- `e2e/helpers/vai-api.ts` — update `listIssues`, `listWorkspaces` to handle envelope

**Tasks:**
- Install nuqs: `pnpm add nuqs`
- Create shared pagination hook or helper that syncs TanStack Table state ↔ nuqs URL params ↔ TanStack Query fetch params
- Update each table component:
  - `manualPagination: true`, `manualSorting: true` on useReactTable
  - `pageCount` from `pagination.total_pages`
  - `onPaginationChange` / `onSortingChange` write to nuqs
  - Pagination UI controls (page numbers, per_page selector)
- Update list hooks to pass `page`, `per_page`, `sort` as query params
- Update hooks to unwrap `response.data` from the envelope
- Update e2e test helpers to handle the new response format
- Add/update unit tests for hooks

**Acceptance criteria:**
- All list pages show paginated tables with page controls
- Changing page/sort updates the URL (e.g. `?page=2&sort=created_at:desc`)
- Refreshing the page preserves pagination state from URL
- Sharing a URL with pagination params works
- `pnpm test` passes
- `pnpm test:e2e` passes
- `npx tsc --noEmit` clean
