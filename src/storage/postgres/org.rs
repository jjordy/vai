//! OrgStore implementation for PostgresStorage.
//!
//! Handles organizations, users, org membership, repo collaborators, and
//! related role-resolution queries. All multi-tenant identity and access
//! management data lives here.

use async_trait::async_trait;
use sqlx::Row;
use uuid::Uuid;

use super::super::{
    NewOrg, NewUser, OrgMember, OrgRole, OrgStore, RepoCollaborator, RepoMember, RepoRole,
    StorageError, User, Organization,
};
use super::PostgresStorage;

#[async_trait]
impl OrgStore for PostgresStorage {
    // ── Organizations ──────────────────────────────────────────────────────────

    async fn create_org(&self, org: NewOrg) -> Result<Organization, StorageError> {
        let row = sqlx::query(
            "INSERT INTO organizations (name, slug)
             VALUES ($1, $2)
             RETURNING id, name, slug, created_at",
        )
        .bind(&org.name)
        .bind(&org.slug)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint().is_some() {
                    return StorageError::Conflict(format!("slug '{}' already exists", org.slug));
                }
            }
            StorageError::Database(e.to_string())
        })?;

        Ok(Organization {
            id: row.get("id"),
            name: row.get("name"),
            slug: row.get("slug"),
            created_at: row.get("created_at"),
        })
    }

    async fn get_org(&self, org_id: &Uuid) -> Result<Organization, StorageError> {
        let row = sqlx::query(
            "SELECT id, name, slug, created_at FROM organizations WHERE id = $1",
        )
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("org {org_id}")))?;

        Ok(Organization {
            id: row.get("id"),
            name: row.get("name"),
            slug: row.get("slug"),
            created_at: row.get("created_at"),
        })
    }

    async fn get_org_by_slug(&self, slug: &str) -> Result<Organization, StorageError> {
        let row = sqlx::query(
            "SELECT id, name, slug, created_at FROM organizations WHERE slug = $1",
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("org with slug '{slug}'")))?;

        Ok(Organization {
            id: row.get("id"),
            name: row.get("name"),
            slug: row.get("slug"),
            created_at: row.get("created_at"),
        })
    }

    async fn list_orgs(&self) -> Result<Vec<Organization>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, name, slug, created_at FROM organizations ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|row| Organization {
                id: row.get("id"),
                name: row.get("name"),
                slug: row.get("slug"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn delete_org(&self, org_id: &Uuid) -> Result<(), StorageError> {
        let result = sqlx::query("DELETE FROM organizations WHERE id = $1")
            .bind(org_id)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(format!("org {org_id}")));
        }
        Ok(())
    }

    // ── Users ─────────────────────────────────────────────────────────────────

    async fn create_user(&self, user: NewUser) -> Result<User, StorageError> {
        let row = sqlx::query(
            "INSERT INTO users (email, name)
             VALUES ($1, $2)
             RETURNING id, email, name, created_at",
        )
        .bind(&user.email)
        .bind(&user.name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint().is_some() {
                    return StorageError::Conflict(format!(
                        "email '{}' already exists",
                        user.email
                    ));
                }
            }
            StorageError::Database(e.to_string())
        })?;

        Ok(User {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            created_at: row.get("created_at"),
        })
    }

    async fn get_user(&self, user_id: &Uuid) -> Result<User, StorageError> {
        let row = sqlx::query(
            "SELECT id, email, name, created_at FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("user {user_id}")))?;

        Ok(User {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            created_at: row.get("created_at"),
        })
    }

    async fn get_user_by_email(&self, email: &str) -> Result<User, StorageError> {
        let row = sqlx::query(
            "SELECT id, email, name, created_at FROM users WHERE email = $1",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("user with email '{email}'")))?;

        Ok(User {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            created_at: row.get("created_at"),
        })
    }

    // ── Org membership ────────────────────────────────────────────────────────

    async fn add_org_member(
        &self,
        org_id: &Uuid,
        user_id: &Uuid,
        role: OrgRole,
    ) -> Result<OrgMember, StorageError> {
        let row = sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role)
             VALUES ($1, $2, $3)
             RETURNING org_id, user_id, role, created_at",
        )
        .bind(org_id)
        .bind(user_id)
        .bind(role.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint().is_some() {
                    return StorageError::Conflict(format!(
                        "user {user_id} is already a member of org {org_id}"
                    ));
                }
            }
            StorageError::Database(e.to_string())
        })?;

        Ok(OrgMember {
            org_id: row.get("org_id"),
            user_id: row.get("user_id"),
            role: OrgRole::from_db_str(row.get("role")),
            created_at: row.get("created_at"),
        })
    }

    async fn update_org_member(
        &self,
        org_id: &Uuid,
        user_id: &Uuid,
        role: OrgRole,
    ) -> Result<OrgMember, StorageError> {
        let row = sqlx::query(
            "UPDATE org_members SET role = $3
             WHERE org_id = $1 AND user_id = $2
             RETURNING org_id, user_id, role, created_at",
        )
        .bind(org_id)
        .bind(user_id)
        .bind(role.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| {
            StorageError::NotFound(format!("membership for user {user_id} in org {org_id}"))
        })?;

        Ok(OrgMember {
            org_id: row.get("org_id"),
            user_id: row.get("user_id"),
            role: OrgRole::from_db_str(row.get("role")),
            created_at: row.get("created_at"),
        })
    }

    async fn remove_org_member(&self, org_id: &Uuid, user_id: &Uuid) -> Result<(), StorageError> {
        let result =
            sqlx::query("DELETE FROM org_members WHERE org_id = $1 AND user_id = $2")
                .bind(org_id)
                .bind(user_id)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(format!(
                "membership for user {user_id} in org {org_id}"
            )));
        }
        Ok(())
    }

    async fn list_org_members(&self, org_id: &Uuid) -> Result<Vec<OrgMember>, StorageError> {
        let rows = sqlx::query(
            "SELECT org_id, user_id, role, created_at
             FROM org_members WHERE org_id = $1
             ORDER BY created_at",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|row| OrgMember {
                org_id: row.get("org_id"),
                user_id: row.get("user_id"),
                role: OrgRole::from_db_str(row.get("role")),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    // ── Org-scoped repo lookup ────────────────────────────────────────────────

    async fn get_repo_id_in_org(
        &self,
        org_id: &Uuid,
        repo_name: &str,
    ) -> Result<Uuid, StorageError> {
        let row = sqlx::query("SELECT id FROM repos WHERE org_id = $1 AND name = $2")
            .bind(org_id)
            .bind(repo_name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        match row {
            Some(r) => Ok(r.get::<Uuid, _>("id")),
            None => Err(StorageError::NotFound(format!(
                "repo '{}' not found in org",
                repo_name
            ))),
        }
    }

    // ── Repo collaborators ────────────────────────────────────────────────────

    async fn add_collaborator(
        &self,
        repo_id: &Uuid,
        user_id: &Uuid,
        role: RepoRole,
    ) -> Result<RepoCollaborator, StorageError> {
        let row = sqlx::query(
            "INSERT INTO repo_collaborators (repo_id, user_id, role)
             VALUES ($1, $2, $3)
             RETURNING repo_id, user_id, role, created_at",
        )
        .bind(repo_id)
        .bind(user_id)
        .bind(role.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint().is_some() {
                    return StorageError::Conflict(format!(
                        "user {user_id} is already a collaborator on repo {repo_id}"
                    ));
                }
            }
            StorageError::Database(e.to_string())
        })?;

        Ok(RepoCollaborator {
            repo_id: row.get("repo_id"),
            user_id: row.get("user_id"),
            role: RepoRole::from_db_str(row.get("role")),
            created_at: row.get("created_at"),
        })
    }

    async fn update_collaborator(
        &self,
        repo_id: &Uuid,
        user_id: &Uuid,
        role: RepoRole,
    ) -> Result<RepoCollaborator, StorageError> {
        let row = sqlx::query(
            "UPDATE repo_collaborators SET role = $3
             WHERE repo_id = $1 AND user_id = $2
             RETURNING repo_id, user_id, role, created_at",
        )
        .bind(repo_id)
        .bind(user_id)
        .bind(role.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| {
            StorageError::NotFound(format!(
                "collaborator {user_id} on repo {repo_id}"
            ))
        })?;

        Ok(RepoCollaborator {
            repo_id: row.get("repo_id"),
            user_id: row.get("user_id"),
            role: RepoRole::from_db_str(row.get("role")),
            created_at: row.get("created_at"),
        })
    }

    async fn remove_collaborator(
        &self,
        repo_id: &Uuid,
        user_id: &Uuid,
    ) -> Result<(), StorageError> {
        let result = sqlx::query(
            "DELETE FROM repo_collaborators WHERE repo_id = $1 AND user_id = $2",
        )
        .bind(repo_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(format!(
                "collaborator {user_id} on repo {repo_id}"
            )));
        }
        Ok(())
    }

    async fn list_collaborators(
        &self,
        repo_id: &Uuid,
    ) -> Result<Vec<RepoCollaborator>, StorageError> {
        let rows = sqlx::query(
            "SELECT repo_id, user_id, role, created_at
             FROM repo_collaborators WHERE repo_id = $1
             ORDER BY created_at",
        )
        .bind(repo_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|row| RepoCollaborator {
                repo_id: row.get("repo_id"),
                user_id: row.get("user_id"),
                role: RepoRole::from_db_str(row.get("role")),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn search_repo_members(
        &self,
        repo_id: &Uuid,
        query: &str,
        limit: i64,
    ) -> Result<Vec<RepoMember>, StorageError> {
        // Build a LIKE pattern: prefix match, case-insensitive via ILIKE.
        let pattern = format!("{}%", query);

        // Union of:
        //   1. Direct repo collaborators (via repo_collaborators JOIN users)
        //   2. Org members (via repos → org_members JOIN users), when the repo
        //      belongs to an org
        //   3. Agent API keys scoped to this repo (not revoked)
        // Deduplicated by (id, member_type), ordered by name, limited.
        let rows = sqlx::query(
            r#"
            SELECT id, name, member_type FROM (
                -- Direct collaborators
                SELECT u.id::text AS id, u.name AS name, 'human' AS member_type
                FROM repo_collaborators rc
                JOIN users u ON u.id = rc.user_id
                WHERE rc.repo_id = $1
                  AND (u.name ILIKE $2 OR u.email ILIKE $2)

                UNION

                -- Org members (when this repo belongs to an org)
                SELECT u.id::text AS id, u.name AS name, 'human' AS member_type
                FROM repos r
                JOIN org_members om ON om.org_id = r.org_id
                JOIN users u ON u.id = om.user_id
                WHERE r.id = $1
                  AND (u.name ILIKE $2 OR u.email ILIKE $2)

                UNION

                -- Agent API keys scoped to this repo
                SELECT ak.id::text AS id, ak.name AS name, 'agent' AS member_type
                FROM api_keys ak
                WHERE ak.repo_id = $1
                  AND NOT ak.revoked
                  AND ak.name ILIKE $2
            ) AS combined
            ORDER BY name
            LIMIT $3
            "#,
        )
        .bind(repo_id)
        .bind(&pattern)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|row| RepoMember {
                id: row.get("id"),
                name: row.get("name"),
                member_type: row.get("member_type"),
            })
            .collect())
    }

    async fn list_repo_ids_for_org(&self, org_id: &Uuid) -> Result<Vec<Uuid>, StorageError> {
        let rows = sqlx::query("SELECT id FROM repos WHERE org_id = $1 ORDER BY created_at")
            .bind(org_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows.iter().map(|r| r.get::<Uuid, _>("id")).collect())
    }

    async fn list_all_repo_ids(&self) -> Result<Vec<Uuid>, StorageError> {
        let rows = sqlx::query("SELECT id FROM repos ORDER BY created_at")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows.iter().map(|r| r.get::<Uuid, _>("id")).collect())
    }

    async fn count_collaborator_repos(&self, user_id: &Uuid) -> Result<u64, StorageError> {
        let row =
            sqlx::query("SELECT COUNT(*) AS n FROM repo_collaborators WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

        let n: i64 = row.get("n");
        Ok(n as u64)
    }

    async fn count_repos_owned_by_user(&self, user_id: &Uuid) -> Result<u64, StorageError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM repo_collaborators WHERE user_id = $1 AND role = 'admin'",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let n: i64 = row.get("n");
        Ok(n as u64)
    }

    async fn resolve_repo_role(
        &self,
        user_id: &Uuid,
        repo_id: &Uuid,
    ) -> Result<Option<RepoRole>, StorageError> {
        // Single query: join repos → org_members (via org_id) and
        // repo_collaborators, both as LEFT JOINs so we always get a row when
        // the repo exists.
        let row = sqlx::query(
            "SELECT om.role  AS org_role,
                    rc.role  AS direct_role
             FROM   repos r
             LEFT JOIN org_members om
                    ON r.org_id IS NOT NULL
                   AND r.org_id   = om.org_id
                   AND om.user_id = $1
             LEFT JOIN repo_collaborators rc
                    ON rc.repo_id  = $2
                   AND rc.user_id  = $1
             WHERE  r.id = $2",
        )
        .bind(user_id)
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let row = match row {
            Some(r) => r,
            // Repo not found — treat as no access.
            None => return Ok(None),
        };

        // Derive an effective role from org membership (owner/admin only).
        let org_derived: Option<RepoRole> = {
            let org_role: Option<&str> = row.get("org_role");
            match org_role {
                Some("owner") => Some(RepoRole::Owner),
                Some("admin") => Some(RepoRole::Admin),
                // org members (role = "member") need a direct collaborator entry.
                _ => None,
            }
        };

        // Direct collaborator role on this specific repo.
        let direct: Option<RepoRole> = {
            let direct_role: Option<&str> = row.get("direct_role");
            direct_role.map(RepoRole::from_db_str)
        };

        // Return the most permissive of the two, or None if neither exists.
        Ok(match (org_derived, direct) {
            (Some(a), Some(b)) => Some(RepoRole::max(a, b)),
            (Some(r), None) | (None, Some(r)) => Some(r),
            (None, None) => None,
        })
    }

    async fn get_or_create_personal_org(
        &self,
        user_id: &Uuid,
        user_name: &str,
    ) -> Result<Organization, StorageError> {
        let slug = format!("user-{}", user_id);
        // Upsert the org row: if the slug already exists keep it unchanged.
        let row = sqlx::query(
            "INSERT INTO organizations (name, slug)
             VALUES ($1, $2)
             ON CONFLICT (slug) DO UPDATE SET slug = EXCLUDED.slug
             RETURNING id, name, slug, created_at",
        )
        .bind(user_name)
        .bind(&slug)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let org = Organization {
            id: row.get("id"),
            name: row.get("name"),
            slug: row.get("slug"),
            created_at: row.get("created_at"),
        };

        // Ensure the user is owner of their personal org (idempotent).
        sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role)
             VALUES ($1, $2, 'owner')
             ON CONFLICT (org_id, user_id) DO NOTHING",
        )
        .bind(org.id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(org)
    }
}
