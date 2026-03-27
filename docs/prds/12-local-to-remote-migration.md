# Phase 12: Local to Remote Migration

## Summary

Provide a seamless migration path from local SQLite-based vai to a hosted Postgres-backed server. Users run one command and their entire history (events, issues, versions) is transferred to the remote server.

## Motivation

Users start with vai locally (zero dependencies, instant setup). When they're ready for collaboration or hosted features, they shouldn't lose their history. The migration must be atomic, fast, and preserve all data relationships.

## Requirements

### 12.1: Migration CLI Command

```bash
vai remote migrate
```

Prerequisites:
- A remote must be configured (`vai remote add <url> --key <key>`)
- The repo must exist on the remote (created via `POST /api/repos`)

The command:
1. Reads all local data from `.vai/`
2. Streams it to the remote server via a bulk endpoint
3. On success, prints a summary and confirms the remote is now active
4. From this point, all CLI commands proxy to the remote

### 12.2: Bulk Migration Endpoint

```
POST /api/repos/:repo/migrate
Content-Type: application/json

{
  "events": [...],
  "versions": [...],
  "issues": [...],
  "escalations": [...],
  "head_version": "v42"
}
```

The server:
1. Validates the payload structure
2. Inserts all data in a single Postgres transaction
3. Returns 200 with a summary on success, or 400/500 with details on failure
4. Rejects migration if the repo already has data (prevent accidental double-migration)

### 12.3: Source File Upload

After the metadata migration, the command uploads all source files:
1. Walk the project directory (respecting `vai.toml` ignore patterns)
2. Upload files via `POST /api/repos/:repo/files` in batches
3. The server stores them in S3/MinIO

This can be done incrementally — upload in batches of 50 files to avoid timeout issues on large repos.

### 12.4: Graph Rebuild

The semantic graph is NOT migrated — it's rebuilt server-side after source files are uploaded:
1. Server triggers `graph refresh` after file upload completes
2. The graph is rebuilt from the uploaded source files
3. This ensures the graph is consistent with the server's parser version

### 12.5: Post-Migration Verification

After migration completes:
1. `vai remote status` confirms connectivity and data
2. `vai status` (proxied to remote) shows the same HEAD version and issue counts as local
3. Local `.vai/` directory is kept as a backup (not deleted)
4. A `.vai/migrated_at` marker file is written with the migration timestamp and remote URL

### 12.6: Rollback

If the user wants to revert to local mode:
```bash
vai remote remove
```

This removes the `[remote]` config. Since local `.vai/` is preserved, CLI commands fall back to local storage. No data is lost on either side.

## Out of Scope

- Bidirectional sync (remote → local is just `vai clone`)
- Incremental migration (migrate only new data since last migration)
- Migration of workspace overlays (active workspaces should be submitted or discarded before migrating)
- Conflict resolution between local and remote data

## Issues

1. **Implement `vai remote migrate` CLI command** — Read all local data, validate remote connection, stream to bulk endpoint, write migration marker. Priority: high.

2. **Implement bulk migration server endpoint** — `POST /api/repos/:repo/migrate` accepts events, versions, issues, escalations. Insert in single transaction. Reject if repo has existing data. Priority: high.

3. **Implement source file upload during migration** — Walk project directory, batch upload files to remote, trigger server-side graph rebuild after upload completes. Priority: high.

4. **Add post-migration verification** — Compare local and remote state after migration. Print summary of transferred data. Keep local `.vai/` as backup. Priority: medium.

5. **Add migration integration test** — Create a local repo with data, migrate to a test Postgres server, verify all data transferred correctly. Priority: medium.
