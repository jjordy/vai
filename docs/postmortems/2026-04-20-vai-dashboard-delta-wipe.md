# Post-Mortem: vai-dashboard v215 — Delta Upload Wiped 439 Files

**Date:** 2026-04-20  
**Severity:** P0 — production data loss  
**Status:** Resolved  

---

## Summary

A RALPH submit on the vai-dashboard repository catastrophically deleted 439 files (all but the 25 files RALPH was modifying). The repository was effectively wiped at version v215. Files were recovered from the previous version in S3, but approximately 40 minutes of developer work was lost.

---

## Timeline

| Time (UTC) | Event |
|---|---|
| ~09:12 | RALPH claims issue, downloads workspace |
| ~09:14 | RALPH edits 25 files in `./work` |
| ~09:16 | `vai agent submit ./work` invoked |
| ~09:16 | Server receives full-mode tarball containing only 25 files |
| ~09:16 | Server marks 439 `current/` files as deleted (not in submitted tarball) |
| ~09:16 | v215 created; 439 files absent from repo HEAD |
| ~09:52 | Developer pulls latest, notices 439 files missing |
| ~10:01 | Incident declared P0; manual recovery from v214 snapshot begins |
| ~10:34 | Recovery complete, v216 restores the 25 RALPH changes on top of v214 |

---

## Root Cause

The `vai agent submit` code path (`src/agent/mod.rs: build_agent_tarball`) packs **only the files present in the submitted work directory** (`./work`). When a RALPH agent downloads a workspace, it receives only the files relevant to its task — not the entire repository. The submitted tarball therefore contained only 25 files.

On the server, `upload_snapshot_handler` (`src/server/workspace.rs`) handles a tarball with no `.vai-delta.json` as **full-mode**: any file in `current/` that is not present in the submitted tarball is recorded as deleted. With 25 files in the tarball and 464 in `current/`, 439 files were silently deleted.

There was no guard against this outcome. The server accepted the upload and applied the deletions without any confirmation or threshold check.

### Why the agent work directory doesn't contain all repo files

`vai agent download` fetches only the files listed in the claimed issue, not a full checkout. This is intentional — agents work on narrow slices. However, the submit path did not compensate for this: it sent a full-mode tarball that implicitly claimed "these 25 files are the entire repository."

### Contributing factors

1. **No deletion safety rail.** The server applied deletions with no guard against mass deletes. A single bad upload erases all history-forward data.
2. **Full-mode semantics on partial checkouts.** The `build_agent_tarball` function uses full-mode packaging even when it only has a subset of the repo. Full-mode is correct when the tarball contains the entire working directory; it is catastrophically wrong when it is a partial set.
3. **No pre-submit review.** RALPH did not verify the file count in the tarball against the known repo size before submitting.

---

## Impact

- **439 files deleted** from vai-dashboard HEAD at v215.
- **~40 minutes of developer work lost** (commits made between v214 and the incident).
- **Manual recovery required**: engineers had to diff v214 vs v215, layer RALPH's 25 changes on top of v214, and create v216.
- **No S3 data loss**: content-addressed blobs (`blobs/{sha256}`) are ref-counted and protected from deletion; recovery was possible.

---

## Resolution

### Immediate (shipped in this PR)

**Server-side safety rail** (`src/server/workspace.rs`):

```
if !query.allow_destructive && deleted > 0 && current_count > 0
   && deleted * 2 > current_count {
    return Err(ApiError::conflict(...));
}
```

`POST /workspaces/{id}/upload-snapshot` now returns **409 Conflict** when the upload would delete more than 50% of current repository files. The caller must pass `?allow_destructive=true` to proceed. This is a hard server-side gate — no client-side change can bypass it.

### Follow-up (separate issues)

- **Agent submit should use delta mode.** `vai agent submit` should build a delta tarball listing only the files it modified, rather than a full-mode tarball of a partial checkout. See issue #305.
- **Pre-submit file count check.** `vai agent submit` should warn (or abort) if `tarball_file_count / repo_file_count < 0.3`. See issue #305.
- **Review `build_full_tarball` exclusions.** The exclusion list (`target`, `node_modules`) differs from `build_agent_tarball` (`target`, `node_modules`, `dist`, `__pycache__`). Files in `dist/` written via workspace submit can accumulate in `current/` and get detected as deleted by an agent submit. Align exclusion lists.

---

## Regression Tests Added

Three new E2E tests in `tests/server_postgres_e2e.rs`:

- **`test_delta_preserves_unchanged_files`**: Seeds 100 files, submits a delta touching 1, asserts all 100 survive.
- **`test_delta_safety_rail_rejects_mass_delete`**: Seeds 100 files, submits a full-mode tarball with only 1 file, asserts 409. Also confirms `?allow_destructive=true` allows the upload.
- **`test_delta_preserves_chain_reconstruction`**: Three chained delta submissions; asserts historical version download returns correct state.

Two new unit tests in `src/remote_workspace.rs`:

- **`build_delta_tarball_excludes_repo_only_files`**: Delta tarball must contain only overlay files + manifest.
- **`build_delta_tarball_deleted_paths_in_manifest_only`**: Deleted paths appear in the manifest JSON only, never as tar entries.

---

## Action Items

| # | Owner | Action | Status |
|---|---|---|---|
| 1 | RALPH | Server safety rail (>50% delete → 409) | **Done** (this PR) |
| 2 | RALPH | Regression tests (E2E + unit) | **Done** (this PR) |
| 3 | Team | Issue #305: Agent submit → delta mode | Open |
| 4 | Team | Issue #305: Align `build_full_tarball` / `build_agent_tarball` exclusions | Open |
| 5 | Team | Runbook: "repo missing files" recovery procedure | Open |

---

## Lessons Learned

1. **Destructive server operations need explicit opt-in.** A full-replace semantic (full-mode upload) should require confirmation when the "replacement" is dramatically smaller than what it replaces.
2. **Partial checkouts are dangerous with full-mode semantics.** Any agent or tool that operates on a subset of a repository must use delta-mode submission, not full-mode.
3. **Content-addressable blobs saved the day.** Because S3 objects are ref-counted and never deleted eagerly, v214 was intact and recovery was straightforward. This design decision proved its worth.
