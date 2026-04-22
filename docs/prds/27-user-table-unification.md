# PRD 27 — User Table Unification

**Status**: Draft (2026-04-22)
**Owner**: Jordy + vai RALPH (execution split — see Ownership section)
**Blocks**: Clean signup flow, #305-class bugs, future Better Auth plugin adoption

## Problem

Two user tables coexist in one Postgres database:

| Table | id type | id format | Rows | FKs into it |
|---|---|---|---|---|
| `user` (Better Auth) | text | nanoid (`ABxdMidsWcpK…`) | 9 | `session.userId`, `account.userId`, `user_onboarding.user_id` |
| `users` (vai Rust) | uuid | uuid (`6f7e5e3e-5cd1-…`) | 8 | `api_keys`, `cli_device_codes`, `org_members`, `refresh_tokens`, `repo_collaborators` |

The tables are bridged by `users.better_auth_id` (text, unique). On first authenticated request, `src/server/auth.rs::token_exchange_handler` calls `get_user_by_external_id()` → `NotFound` → `create_user()` to insert the matching `users` row. Introduced in commit `1e10647` (2026-04-01).

### Why this is broken

1. **Lazy provisioning is race-unsafe.** Check-then-insert on concurrent token exchanges hits the `better_auth_id` UNIQUE constraint and returns 500.
2. **Orphan users from before the bridge existed.** `jordy@vai.dev` (created 2026-03-24, before the bridge migration on 2026-04-01) has no `users` row.
3. **Ad-hoc migration tooling.** Only migration in `vai-dashboard/migrations/` is `0001_user_onboarding.sql`, applied manually once. Better Auth's schema (`user`, `session`, `account`, `verification`) was applied via `npx @better-auth/cli migrate` in some past manual step. No CI path for future Better Auth schema changes.
4. **Every `users` predicate detour.** Every domain query that wants to know "who is this user" has to go through `better_auth_id` — the JOIN layer is a known source of bugs (#305 chain) and makes onboarding predicates fragile (RFC `7348a516`).
5. **FK type split.** Three tables FK to `user(id) TEXT`, five FK to `users(id) UUID`. Any cross-table join becomes a type cast.

## Target state

One user table, `users`, with uuid ids, owned by Better Auth via its config:

```ts
// vai-dashboard/src/lib/auth.ts
export const auth = betterAuth({
  advanced: { database: { generateId: "uuid" } },
  user: {
    modelName: "users",                // Better Auth writes to `users`, not `user`
    fields: {
      emailVerified: "email_verified", // map camelCase → snake_case
      createdAt: "created_at",
      updatedAt: "updated_at",
    },
    // no additionalFields — domain data lives in separate tables keyed on users.id
  },
  // session / account / verification stay as-is (Better Auth owns them)
});
```

Downstream:
- All 7 FK tables reference `users(id) UUID`. Unified type throughout.
- `better_auth_id` column dropped; `users.id` IS the Better Auth user id.
- `get_user_by_external_id()` deleted; replaced by direct `SELECT FROM users WHERE id = $1`.
- Auto-provisioning in `token_exchange_handler` deleted; Better Auth writes the `users` row on signup directly.
- Custom fields (onboarding, future per-user settings) live in separate tables keyed on `users.id (uuid)`, matching `user_onboarding`'s pattern.

## Non-goals

- Changing Better Auth's authentication flow (email/password + GitHub OAuth stay as-is).
- Replacing React Query or the dashboard's state library.
- Adding new user fields beyond what Better Auth's default schema provides.
- Touching `repo_collaborators.role` semantics.

## Migration strategy

Single-shot destructive migration. Safe because:
- Only one real user (`jordanrileyaddison@gmail.com`). Others are e2e test accounts (auto-recreate on next e2e run) or orphans.
- Full DB backup taken before migration.
- Migration is idempotent-by-consequence: if re-run on the target schema, it's a no-op.

### Phase 0 — Pre-flight (manual, before any phases run)

1. Take a full Postgres backup:
   ```
   fly postgres connect -a vai-postgres -c 'pg_dump -d vai_server_polished_feather_2668' > /home/jordy/development/backups/pre-prd27-$(date +%Y%m%d).sql
   ```
2. Stop incoming traffic (optional — set Fly machine to read-only or put up a maintenance page if concerned about concurrent writes during the 2-minute migration).

### Phase 1 — Data migration (manual one-shot SQL)

`vai/migrations/20260422000000_unify_user_tables.sql` (applied ONCE manually via `fly postgres connect`, not via sqlx auto-migrate — this is a data migration, not a schema migration. After it runs, subsequent deploys see the target schema and don't re-run this step.)

```sql
BEGIN;

-- 1. Add Better Auth columns to `users` so it can serve as the unified user table.
ALTER TABLE users ADD COLUMN IF NOT EXISTS email_verified BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS image TEXT;
ALTER TABLE users ADD COLUMN IF NOT EXISTS updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW();

-- 2. Backfill from Better Auth user table for the kept user.
UPDATE users u
   SET email_verified = ba."emailVerified",
       image          = ba.image,
       updated_at     = ba."updatedAt"
  FROM "user" ba
 WHERE ba.id = u.better_auth_id
   AND ba.email = 'jordanrileyaddison@gmail.com';

-- 3. Remove every non-jordanrileyaddison Better Auth user (CASCADE handles session/account/user_onboarding).
DELETE FROM "user" WHERE email != 'jordanrileyaddison@gmail.com';

-- 4. Remove the corresponding `users` row for jordy@vai.dev-style orphans.
--    Anyone without a matching Better Auth row is by definition orphaned now.
DELETE FROM users WHERE better_auth_id IS NULL OR NOT EXISTS (
  SELECT 1 FROM "user" ba WHERE ba.id = users.better_auth_id
);

-- 5. Align `user.id` with `users.id` for the kept user.
--    The surviving users.id is a UUID. Better Auth currently has a nanoid.
--    We force Better Auth's id to match users.id so future FKs agree.
UPDATE "user"
   SET id = u.id::text
  FROM users u
 WHERE "user".id = u.better_auth_id;
--   session.userId / account.userId / user_onboarding.user_id CASCADE-updated via ON UPDATE CASCADE?
--   CHECK: they might NOT have ON UPDATE CASCADE. If not, step 5a is needed:

-- 5a. (Only if the FK constraints lack ON UPDATE CASCADE):
UPDATE session   SET "userId" = u.id::text FROM users u WHERE session."userId" = u.better_auth_id;
UPDATE account   SET "userId" = u.id::text FROM users u WHERE account."userId" = u.better_auth_id;
UPDATE user_onboarding SET user_id = u.id::text FROM users u WHERE user_onboarding.user_id = u.better_auth_id;

-- 6. Change FK column types. Postgres can cast text→uuid when values are valid uuid strings.
ALTER TABLE "user"           ALTER COLUMN id      TYPE UUID USING id::uuid;
ALTER TABLE session          ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid;
ALTER TABLE account          ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid;
ALTER TABLE user_onboarding  ALTER COLUMN user_id  TYPE UUID USING user_id::uuid;

-- 7. Drop the now-redundant `user` table — Better Auth will write to `users` going forward.
DROP TABLE "user";

-- 8. Drop the bridge column; users.id is now the Better Auth id directly.
ALTER TABLE users DROP COLUMN better_auth_id;

-- 9. Drop any other stale Better Auth columns we don't want (e.g., vaiApiKey if it still exists on `users`).
ALTER TABLE users DROP COLUMN IF EXISTS "vaiApiKey";

COMMIT;
```

**Gotchas this migration deliberately avoids:**
- No attempt to preserve sessions. The 268 session rows for non-kept users are wiped by the CASCADE on step 3. Jordy's session survives only if his Better Auth nanoid happens to cast to a uuid — almost certainly not. **Expect to re-login once after migration.**
- No attempt to preserve GitHub OAuth linking for deleted accounts. If those users ever return, they re-sign up.
- Step 5a will be UNSKIPPABLE if the FK constraints lack `ON UPDATE CASCADE`. Verify before running:
  ```sql
  SELECT tc.constraint_name, tc.table_name, rc.update_rule
    FROM information_schema.table_constraints tc
    JOIN information_schema.referential_constraints rc ON tc.constraint_name = rc.constraint_name
   WHERE tc.table_name IN ('session','account','user_onboarding');
  ```

### Phase 2 — Code changes (RALPH-able once data migration is done)

Split across two RALPH loops:

**vai Rust (jjordy/vai GitHub issues — vai RALPH loop):**

- [ ] Remove `get_user_by_external_id` from `src/storage/*` (both postgres + sqlite impls + trait).
- [ ] Remove `create_user` auto-provisioning branch in `src/server/auth.rs::token_exchange_handler`. Better Auth is now authoritative. The handler's only job is to validate the session and mint a JWT — if the session validates, the `users.id` is directly the BA user id, no lookup needed.
- [ ] Update all callers that queried `users` via `better_auth_id` to use `users.id` directly. Grep for `better_auth_id`.
- [ ] Remove the `better_auth_id` column from any Rust `User` struct in `src/storage/org.rs` and similar.
- [ ] Add a sqlx migration `20260423000000_drop_better_auth_id.sql` that's a no-op if phase 1 already ran, but is the canonical record of the column being gone. (Migrations table accommodates this via the checksum.)
- [ ] Delete the auto-grant-all-repos logic if any still remains from commit `f223094` — the whole reason that existed was the old bridge pattern.
- [ ] Update `src/server/auth.rs` log events to remove `better_auth_id` references.

**vai-dashboard (vai-dashboard issues — dashboard RALPH loop):**

- [ ] Update `src/lib/auth.ts` with:
  ```ts
  advanced: { database: { generateId: "uuid" } },
  user: {
    modelName: "users",
    fields: {
      emailVerified: "email_verified",
      createdAt: "created_at",
      updatedAt: "updated_at",
    },
  },
  ```
- [ ] Ensure no `additionalFields` block is present (confirm #318 cleanup held).
- [ ] Update `e2e/auth.setup.ts` to remove the stale `vaiApiKey: "admin-secret-key-123"` from signup payload and the follow-up `update-user` call.
- [ ] Delete `vai-dashboard/migrations/` directory — Better Auth CLI now owns user-adjacent schema, vai server sqlx owns domain tables.

### Phase 3 — Ongoing migration tooling (local script)

Better Auth CLI migrate requires `src/lib/auth.ts` as its config source — it reads the `betterAuth({...})` declaration and applies the implied schema. The CLI must therefore run from a checkout of vai-dashboard, which rules out locating the workflow in the jjordy/vai GitHub repo.

vai-dashboard currently has no GitHub remote (it uses vai for SCM) and no active deploy pipeline, so a GHA-based automation has nowhere to hook in. Rather than block PRD 27 on setting that infrastructure up, Phase 3 ships as a local script that Jordy runs manually whenever `auth.ts` changes. Promoting to CI is a separate PRD tied to the eventual dashboard deploy story.

**New file**: `vai-dashboard/scripts/migrate-prod.sh`

```bash
#!/usr/bin/env bash
# Runs Better Auth CLI migrate against Fly Postgres.
# Prerequisites: flyctl authenticated (`fly auth login`), pnpm install completed.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Start proxy in background
flyctl proxy 5432:5432 -a vai-postgres &
PROXY_PID=$!
trap "kill $PROXY_PID 2>/dev/null || true" EXIT
sleep 5

# Load DATABASE_URL from .env (or accept override from environment).
if [ -z "${DATABASE_URL:-}" ] && [ -f .env ]; then
    set -a; source .env; set +a
fi
if [ -z "${DATABASE_URL:-}" ]; then
    echo "error: DATABASE_URL not set" >&2
    exit 1
fi

echo "==> Running Better Auth migrate against production DB"
npx @better-auth/cli@latest migrate --config src/lib/auth.ts -y

echo "==> Done. Verify the schema with \`psql\` if you changed anything destructive."
```

Usage:
```
cd vai-dashboard
./scripts/migrate-prod.sh
```

The same script is re-usable after initial Phase 1 completes — subsequent runs against the already-unified schema are no-ops unless `auth.ts` changes. When vai-dashboard eventually gets a deploy pipeline (Cloudflare Pages build triggered by something), this script becomes a pre-deploy step in that pipeline; the internals don't change.

**Why not run migrations on the Rust server startup?** Keeps Node off the production Rust runtime. Fly builds are slim; adding Node adds ~150 MB to the deploy image.

### Phase 4 — Cleanup + verification

- [ ] Manual SQL: `SELECT id, email FROM users; SELECT id, email FROM "user";` — confirm `user` is gone, `users` contains the one kept account with Better Auth fields populated.
- [ ] Log in via the dashboard — session re-established, `/api/me` returns expected data.
- [ ] Run `vai init` in a fresh dir — confirm `repo_collaborators` row is created (the whole #305 saga proves this works end-to-end).
- [ ] Run `pnpm test:e2e` in vai-dashboard — auth.setup.ts re-creates `e2e-test@example.com` cleanly; full suite passes.
- [ ] Run `./scripts/migrate-prod.sh` once from vai-dashboard — confirm it's a no-op against the post-migration schema.

## Ownership split

| Phase | Step | Owner |
|---|---|---|
| 0 | DB backup | **Manual (Jordy)** |
| 0 | Rotate Postgres password | **Manual (Jordy)** |
| 0 | Verify FK `ON UPDATE CASCADE` rules | **Manual (Jordy)** |
| 1 | Run data migration SQL on prod | **Manual (Jordy)** |
| 2 | Rust server code changes | **vai RALPH loop** (one GitHub issue per sub-task) |
| 2 | Dashboard `auth.ts` + e2e cleanup | **vai-dashboard RALPH loop** (one vai-dashboard issue per sub-task) |
| 3 | Add `scripts/migrate-prod.sh` to vai-dashboard | **vai-dashboard RALPH loop** |
| 3 | Rotate Postgres password + update `.env` | **Manual (Jordy)** |
| 4 | Verification | **Manual (Jordy)** |

## Rollback

If Phase 1 fails mid-way:
1. Restore from the Phase 0 backup: `fly postgres connect -a vai-postgres < backup.sql`.
2. Abort Phase 2 code changes.
3. Debug offline; re-attempt when understood.

If Phase 2 code reaches production before Phase 1 data migration completes, the server will crash on first query because `better_auth_id` column will be missing. Mitigation: **land Phase 1 first, then deploy Phase 2 code in the same 30-minute window.** Any user active during the window may see errors; acceptable since we're still pre-prod.

## Acceptance

- Exactly one user table (`users`) with uuid ids and Better Auth-aligned column names.
- `better_auth_id` column gone; all code queries `users` by `id` directly.
- `token_exchange_handler` no longer auto-provisions — Better Auth writes the `users` row on signup.
- `scripts/migrate-prod.sh` runs Better Auth CLI migrate against prod cleanly; first run post-migration is a no-op.
- `pnpm test:e2e` passes on the unified schema.
- Zero orphan users post-migration.
- `jordanrileyaddison@gmail.com` can log in and their existing repos + api_keys are intact.

## Related

- RFC #1 (vai-dashboard `2bcfd404`) — unified API client; independent but complementary cleanup.
- RFC #2 (vai-dashboard `7348a516`) — onboarding state consolidation; depends on RFC #1.
- #305 family — the collab-row bugs that prompted this whole investigation.
- Better Auth docs: https://better-auth.com/docs/concepts/database#schema — `modelName`, `fields`, `advanced.database.generateId`.
