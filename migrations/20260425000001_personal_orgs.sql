-- Personal org backfill (PRD 28 Phase 4 prerequisite, Issue #370).
--
-- Every user gets exactly one personal org with slug = 'user-{user_id}'.
-- Existing repos are assigned to their admin collaborator's personal org.
-- New users and repos get a personal org assigned at creation time (in code).

-- 1. Create a personal org for every user that doesn't already have one.
INSERT INTO organizations (id, name, slug, created_at)
SELECT
    gen_random_uuid(),
    u.name,
    'user-' || u.id::text,
    u.created_at
FROM users u
WHERE NOT EXISTS (
    SELECT 1 FROM organizations o WHERE o.slug = 'user-' || u.id::text
);

-- 2. Make each user the owner of their personal org.
INSERT INTO org_members (org_id, user_id, role, created_at)
SELECT o.id, u.id, 'owner', now()
FROM users u
JOIN organizations o ON o.slug = 'user-' || u.id::text
WHERE NOT EXISTS (
    SELECT 1 FROM org_members om WHERE om.org_id = o.id AND om.user_id = u.id
);

-- 3. Backfill repos.org_id: assign each org-less repo to the earliest admin
--    collaborator's personal org.  Repos with no admin collaborator stay NULL
--    and can be reassigned manually.
UPDATE repos
SET org_id = (
    SELECT o.id
    FROM repo_collaborators rc
    JOIN organizations o ON o.slug = 'user-' || rc.user_id::text
    WHERE rc.repo_id = repos.id
      AND rc.role = 'admin'
    ORDER BY rc.created_at ASC
    LIMIT 1
)
WHERE org_id IS NULL;
