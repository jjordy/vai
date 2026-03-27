# PRD 16: Remaining Work Summary

## Open Issues by Phase

### Currently Being Worked (RALPH Active)
- **#150**: Add issue file attachment endpoints
- **#152**: Add issue templates API
- **#154**: Remaining server handlers still use filesystem instead of storage trait

### PRD 15 Phase 1: Foundation (Ready for RALPH)
- **#157**: Add `deleted_paths` to workspace schema and upload endpoint
- **#158**: Seed `current/` prefix in S3 during migration
- **#159**: Download handler serves from `current/` in S3 only
- **#160**: Add tarball upload endpoint for agent workflows

### PRD 15 Phase 2: Merge Engine (Closed, Reopen After Phase 1)
- **#161**: Introduce `MergeFs` trait abstraction
- **#162**: Implement `S3MergeFs` — in-memory buffer backed by S3
- **#163**: Delete `prepare_workspace_for_submit`
- **#164**: Submit handler updates `current/` in S3 after merge

### PRD 15 Phase 3: Handler Cleanup (Closed, Reopen After Phase 2)
- **#165**: E2E tests with read-only `repo_root`
- **#166**: CI grep check for `std::fs` in server handlers
- **#167**: Deletion round-trip integration test

### PRD 15 Phase 5: Agent DX (Closed, Reopen After Phase 3)
- **#168**: Add `WatcherStore` trait and Postgres implementation
- **#169**: Tarball delta mode for large repos

### Other Open Issues
- **#153**: WebSocket Postgres event delivery — keepalive, reconnection, error recovery
- **#156**: Workspace overlay system must track file deletions (covered by #157)

## Issue Overlap / Cleanup

| Issue | Status | Notes |
|-------|--------|-------|
| #153 | Open | Standalone — WebSocket reliability. Can be done anytime. |
| #154 | In Progress | Partially done. Remaining handlers will be fully fixed by Phase 2-3. |
| #155 | Closed | Superseded by #161-#164 (Phase 2). |
| #156 | Open | Covered by #157. Close when #157 is complete. |

## Recommended Execution Order

1. Let RALPH finish #150, #152, #154
2. RALPH works #153 (WebSocket fix — standalone, no dependencies)
3. RALPH works #157, #158, #159, #160 (Phase 1 — foundation)
4. Reopen and work #161, #162, #163, #164 (Phase 2 — merge engine)
5. Reopen and work #165, #166, #167 (Phase 4 — testing)
6. Reopen and work #168, #169 (Phase 5 — agent DX)

## After vai Work is Complete

Once the server-side storage purity work is done:
1. Re-run `vai remote migrate` to seed `current/` in S3
2. Create vai-dashboard issues from `docs/prds/01-orval-migration-completion.md`
3. Create vai-dashboard issues from `docs/prds/02-dashboard-polish.md`
4. Run dashboard RALPH to complete the orval hook migration
5. Verify end-to-end: agent claims issue → downloads repo → works → uploads tarball → submits → new version with correct diffs
