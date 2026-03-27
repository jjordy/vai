# Phase 11: Postgres-Backed Real-Time Event System

## Summary

Replace the NDJSON file-based event log with a Postgres-backed event store for server mode. Use Postgres LISTEN/NOTIFY to drive real-time WebSocket event delivery. Events survive server restarts and support replay from any point.

## Motivation

The current event system uses in-memory tokio broadcast channels — events are lost on server restart, and multiple server instances can't share events. The NDJSON file-based event log is the source of truth but has no real-time notification mechanism. Moving to Postgres as the event store unifies persistence and real-time delivery.

## Requirements

### 11.1: Events Table

```sql
CREATE TABLE events (
    id BIGSERIAL PRIMARY KEY,
    repo_id UUID NOT NULL REFERENCES repos(id),
    event_type TEXT NOT NULL,
    workspace_id UUID,
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Indexes for common query patterns
    INDEX idx_events_repo_type (repo_id, event_type),
    INDEX idx_events_repo_workspace (repo_id, workspace_id),
    INDEX idx_events_repo_created (repo_id, created_at),
    INDEX idx_events_repo_id (repo_id, id)  -- for replay queries
);
```

### 11.2: Write Path

When any operation creates an event (workspace created, issue updated, merge completed, etc.):
1. INSERT into the `events` table
2. Execute `NOTIFY vai_events, '<repo_id>:<event_id>'` to signal WebSocket listeners
3. Return the event with its assigned ID

The NOTIFY payload is lightweight — just repo_id and event_id. The WebSocket handler reads the full event from the table.

### 11.3: WebSocket Delivery

The WebSocket handler:
1. On client connect + subscribe, start listening on the `vai_events` Postgres channel
2. When a NOTIFY arrives for the subscribed repo, query the events table for new events since last delivered ID
3. Apply the client's subscription filter (event types, entities, paths, workspaces)
4. Send matching events to the client

### 11.4: Replay

When a client connects with `?last_event_id=N`:
1. Query `SELECT * FROM events WHERE repo_id = $1 AND id > $2 ORDER BY id`
2. Apply subscription filter
3. Send all matching events, then switch to live NOTIFY-driven delivery

This replaces the in-memory event buffer with durable replay from Postgres.

### 11.5: Local Mode Compatibility

In local SQLite mode, keep the current in-memory broadcast approach. The EventStore trait from PRD 09 handles the abstraction — local mode appends to SQLite and broadcasts in-memory, server mode inserts to Postgres and uses NOTIFY.

## Out of Scope

- Event compaction / archival (future — partition by month, archive to cold storage)
- Cross-repo event aggregation (future — "show me all events across my repos")
- Redis pub/sub (not needed yet — Postgres NOTIFY handles the current scale)

## Issues

1. **Create events table and migration** — Add the events table with proper indexes. Support the full set of event types as TEXT. Priority: high.

2. **Implement Postgres EventStore with LISTEN/NOTIFY** — Write path: INSERT + NOTIFY. Query methods: by type, workspace, time range, since ID. Priority: high.

3. **Update WebSocket handler to use Postgres events** — Replace in-memory broadcast with NOTIFY-driven delivery. Support replay from last_event_id via database query. Priority: high.

4. **Add event delivery filtering in the database layer** — Apply subscription filters (event type, workspace, entity) in the SQL query rather than client-side. Priority: medium.

5. **Add integration tests for Postgres event delivery** — Test append, query, NOTIFY delivery, replay, and filtering against a real Postgres instance. Priority: medium.
