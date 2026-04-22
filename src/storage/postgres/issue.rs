//! IssueStore, CommentStore, IssueLinkStore, and AttachmentStore implementations
//! for PostgresStorage.
//!
//! Covers all CRUD operations for issues, comments (with mentions), issue links,
//! and file attachments, all scoped by `repo_id`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use super::super::{
    AttachmentStore, CommentStore, CommentMention, IssueLinkStore, IssueStore,
    IssueComment, IssueLink, IssueLinkRelationship, IssueUpdate, NewCommentMention,
    NewIssue, NewIssueAttachment, NewIssueComment, NewIssueLink, StorageError,
};
use super::super::pagination::{ListQuery, ListResult};
use super::PostgresStorage;
use crate::issue::{AgentSource, Issue, IssueAttachment, IssueFilter, IssuePriority, IssueStatus};

#[async_trait]
impl IssueStore for PostgresStorage {
    async fn create_issue(&self, repo_id: &Uuid, issue: NewIssue) -> Result<Issue, StorageError> {
        let id = Uuid::new_v4();
        let status = IssueStatus::Open.as_str().to_string();
        let priority = issue.priority.as_str().to_string();
        let agent_source = issue
            .agent_source
            .map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null));

        sqlx::query(
            r#"
            INSERT INTO issues (id, repo_id, title, body, status, priority, labels, creator, agent_source, acceptance_criteria)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(id)
        .bind(repo_id)
        .bind(&issue.title)
        .bind(&issue.description)
        .bind(&status)
        .bind(&priority)
        .bind(&issue.labels)
        .bind(&issue.creator)
        .bind(&agent_source)
        .bind(&issue.acceptance_criteria)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_issue(repo_id, &id).await
    }

    async fn get_issue(&self, repo_id: &Uuid, id: &Uuid) -> Result<Issue, StorageError> {
        let row = sqlx::query(
            "SELECT id, title, body, status, priority, labels, creator, agent_source, \
                    resolution, created_at, updated_at, acceptance_criteria \
             FROM issues WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("issue {id}")))?;

        let issue = row_to_issue(row)?;

        Ok(issue)
    }

    async fn list_issues(
        &self,
        repo_id: &Uuid,
        filter: &IssueFilter,
        query: &ListQuery,
    ) -> Result<ListResult<Issue>, StorageError> {
        // Build dynamic WHERE clause from filter fields.
        let mut conditions = vec!["repo_id = $1".to_string()];
        let mut param_idx = 2usize;

        if filter.status.is_some() {
            conditions.push(format!("LOWER(status) = ${param_idx}"));
            param_idx += 1;
        }
        if filter.priority.is_some() {
            conditions.push(format!("LOWER(priority) = ${param_idx}"));
            param_idx += 1;
        }
        if filter.label.is_some() {
            // Case-insensitive array element match.
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM unnest(labels) l WHERE lower(l) = lower(${param_idx}))"
            ));
            param_idx += 1;
        }
        if filter.creator.is_some() {
            conditions.push(format!("creator = ${param_idx}"));
            param_idx += 1;
        }
        if filter.blocked_by.is_some() {
            conditions.push(format!(
                "id IN (SELECT target_id FROM issue_links WHERE source_id = ${param_idx} AND relationship = 'blocks')"
            ));
            param_idx += 1;
        }
        let _ = param_idx; // suppress unused warning after last use

        let where_clause = conditions.join(" AND ");

        // Build ORDER BY from query sort fields.
        let col_map: HashMap<&str, &str> = [
            ("created_at", "created_at"),
            ("updated_at", "updated_at"),
            ("priority", "priority"),
            ("status", "status"),
            ("title", "title"),
            ("creator", "creator"),
            ("id", "id"),
        ]
        .into_iter()
        .collect();
        let order_by = query.sql_order_by(&col_map);
        let order_by = if order_by.is_empty() {
            "ORDER BY created_at DESC".to_string()
        } else {
            order_by
        };

        let (limit, offset) = query.sql_limit_offset();
        let limit_clause = if limit == i64::MAX {
            String::new()
        } else {
            format!(" LIMIT {limit} OFFSET {offset}")
        };

        let select_sql = format!(
            "SELECT id, title, body, status, priority, labels, creator, agent_source, \
             resolution, created_at, updated_at, acceptance_criteria \
             FROM issues WHERE {where_clause} {order_by}{limit_clause}"
        );
        let count_sql = format!(
            "SELECT COUNT(*) FROM issues WHERE {where_clause}"
        );

        // Bind parameters in the same order as the WHERE clause.
        macro_rules! bind_filter {
            ($q:expr) => {{
                let mut q = $q;
                q = q.bind(*repo_id);
                if let Some(ref s) = filter.status {
                    q = q.bind(s.as_str().to_string());
                }
                if let Some(ref p) = filter.priority {
                    q = q.bind(p.as_str().to_string());
                }
                if let Some(ref l) = filter.label {
                    q = q.bind(l.clone());
                }
                if let Some(ref c) = filter.creator {
                    q = q.bind(c.clone());
                }
                if let Some(ref b) = filter.blocked_by {
                    q = q.bind(*b);
                }
                q
            }};
        }

        let count_row = bind_filter!(sqlx::query(&count_sql))
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let total: i64 = count_row.get(0);

        let rows = bind_filter!(sqlx::query(&select_sql))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let items: Result<Vec<Issue>, StorageError> = rows.into_iter().map(row_to_issue).collect();
        Ok(ListResult { items: items?, total: total as u64 })
    }

    async fn update_issue(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        update: IssueUpdate,
    ) -> Result<Issue, StorageError> {
        let current = self.get_issue(repo_id, id).await?;

        let title = update.title.unwrap_or(current.title);
        let body = update.description.unwrap_or(current.description);
        let priority = update
            .priority
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| current.priority.as_str().to_string());
        let labels = update.labels.unwrap_or(current.labels);
        let status = update
            .status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| current.status.as_str().to_string());
        let resolution = update.resolution.or(current.resolution);
        let acceptance_criteria = update.acceptance_criteria.unwrap_or(current.acceptance_criteria);

        sqlx::query(
            r#"
            UPDATE issues
            SET title = $1, body = $2, priority = $3, labels = $4,
                status = $5, resolution = $6, acceptance_criteria = $7, updated_at = now()
            WHERE repo_id = $8 AND id = $9
            "#,
        )
        .bind(&title)
        .bind(&body)
        .bind(&priority)
        .bind(&labels)
        .bind(&status)
        .bind(&resolution)
        .bind(&acceptance_criteria)
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_issue(repo_id, id).await
    }

    async fn close_issue(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        resolution: &str,
    ) -> Result<Issue, StorageError> {
        sqlx::query(
            "UPDATE issues SET status = $1, resolution = $2, updated_at = now() \
             WHERE repo_id = $3 AND id = $4",
        )
        .bind(IssueStatus::Closed.as_str())
        .bind(resolution)
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_issue(repo_id, id).await
    }
}

fn row_to_issue(row: sqlx::postgres::PgRow) -> Result<Issue, StorageError> {
    let id: Uuid = row.get("id");
    let title: String = row.get("title");
    let description: String = row.get("body");
    let status_str: String = row.get("status");
    let priority_str: String = row.get("priority");
    let labels: Vec<String> = row.get("labels");
    let creator: String = row.get("creator");
    let agent_source_val: Option<serde_json::Value> = row.get("agent_source");
    let resolution: Option<String> = row.get("resolution");
    let created_at: DateTime<Utc> = row.get("created_at");
    let updated_at: DateTime<Utc> = row.get("updated_at");
    let acceptance_criteria: Vec<String> = row.try_get("acceptance_criteria").unwrap_or_default();

    let status = IssueStatus::from_db_str(&status_str).unwrap_or(IssueStatus::Open);
    let priority = IssuePriority::from_db_str(&priority_str).unwrap_or(IssuePriority::Medium);
    let agent_source: Option<AgentSource> = agent_source_val
        .and_then(|v| serde_json::from_value(v).ok());

    Ok(Issue {
        id,
        title,
        description,
        status,
        priority,
        labels,
        creator,
        agent_source,
        resolution,
        acceptance_criteria,
        created_at,
        updated_at,
    })
}

// ── CommentStore ──────────────────────────────────────────────────────────────

#[async_trait]
impl CommentStore for PostgresStorage {
    async fn create_comment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        comment: NewIssueComment,
    ) -> Result<IssueComment, StorageError> {
        let row = sqlx::query(
            r#"
            INSERT INTO issue_comments (repo_id, issue_id, author, body, author_type, author_id, parent_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id, issue_id, author, body, created_at, author_type, author_id, parent_id, edited_at, deleted_at
            "#,
        )
        .bind(repo_id)
        .bind(issue_id)
        .bind(&comment.author)
        .bind(&comment.body)
        .bind(&comment.author_type)
        .bind(&comment.author_id)
        .bind(comment.parent_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(IssueComment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            author: row.get("author"),
            body: row.get("body"),
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            created_at: row.get("created_at"),
            parent_id: row.get("parent_id"),
            edited_at: row.get("edited_at"),
            deleted_at: row.get("deleted_at"),
        })
    }

    async fn list_comments(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueComment>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, issue_id, author, body, created_at, author_type, author_id, parent_id, edited_at, deleted_at \
             FROM issue_comments \
             WHERE repo_id = $1 AND issue_id = $2 \
             ORDER BY created_at ASC",
        )
        .bind(repo_id)
        .bind(issue_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| IssueComment {
                id: row.get("id"),
                issue_id: row.get("issue_id"),
                author: row.get("author"),
                body: row.get("body"),
                author_type: row.get("author_type"),
                author_id: row.get("author_id"),
                created_at: row.get("created_at"),
                parent_id: row.get("parent_id"),
                edited_at: row.get("edited_at"),
                deleted_at: row.get("deleted_at"),
            })
            .collect())
    }

    async fn update_comment(
        &self,
        repo_id: &Uuid,
        comment_id: &Uuid,
        new_body: &str,
    ) -> Result<IssueComment, StorageError> {
        let row = sqlx::query(
            r#"
            UPDATE issue_comments
            SET body = $1, edited_at = now()
            WHERE id = $2 AND repo_id = $3
            RETURNING id, issue_id, author, body, created_at, author_type, author_id, parent_id, edited_at, deleted_at
            "#,
        )
        .bind(new_body)
        .bind(comment_id)
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(comment_id.to_string()))?;

        Ok(IssueComment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            author: row.get("author"),
            body: row.get("body"),
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            created_at: row.get("created_at"),
            parent_id: row.get("parent_id"),
            edited_at: row.get("edited_at"),
            deleted_at: row.get("deleted_at"),
        })
    }

    async fn soft_delete_comment(
        &self,
        repo_id: &Uuid,
        comment_id: &Uuid,
    ) -> Result<IssueComment, StorageError> {
        let row = sqlx::query(
            r#"
            UPDATE issue_comments
            SET deleted_at = now()
            WHERE id = $1 AND repo_id = $2
            RETURNING id, issue_id, author, body, created_at, author_type, author_id, parent_id, edited_at, deleted_at
            "#,
        )
        .bind(comment_id)
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(comment_id.to_string()))?;

        Ok(IssueComment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            author: row.get("author"),
            body: row.get("body"),
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            created_at: row.get("created_at"),
            parent_id: row.get("parent_id"),
            edited_at: row.get("edited_at"),
            deleted_at: row.get("deleted_at"),
        })
    }

    async fn replace_mentions(
        &self,
        repo_id: &Uuid,
        comment_id: &Uuid,
        mentions: Vec<NewCommentMention>,
    ) -> Result<Vec<CommentMention>, StorageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        // Delete existing mentions for this comment.
        sqlx::query("DELETE FROM comment_mentions WHERE comment_id = $1 AND repo_id = $2")
            .bind(comment_id)
            .bind(repo_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let mut result = Vec::with_capacity(mentions.len());
        for m in mentions {
            let row = sqlx::query(
                r#"
                INSERT INTO comment_mentions (comment_id, repo_id, mentioned_user_id, mentioned_key_id, mentioned_name, mention_type)
                VALUES ($1, $2, $3, $4, $5, $6)
                RETURNING id, comment_id, mentioned_user_id, mentioned_key_id, mentioned_name, mention_type
                "#,
            )
            .bind(comment_id)
            .bind(repo_id)
            .bind(m.mentioned_user_id)
            .bind(m.mentioned_key_id)
            .bind(&m.mentioned_name)
            .bind(&m.mention_type)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

            result.push(CommentMention {
                id: row.get("id"),
                comment_id: row.get("comment_id"),
                mentioned_user_id: row.get("mentioned_user_id"),
                mentioned_key_id: row.get("mentioned_key_id"),
                mentioned_name: row.get("mentioned_name"),
                mention_type: row.get("mention_type"),
            });
        }

        tx.commit()
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(result)
    }

    async fn list_mentions(
        &self,
        repo_id: &Uuid,
        comment_id: &Uuid,
    ) -> Result<Vec<CommentMention>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, comment_id, mentioned_user_id, mentioned_key_id, mentioned_name, mention_type \
             FROM comment_mentions WHERE repo_id = $1 AND comment_id = $2 ORDER BY created_at ASC",
        )
        .bind(repo_id)
        .bind(comment_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| CommentMention {
                id: row.get("id"),
                comment_id: row.get("comment_id"),
                mentioned_user_id: row.get("mentioned_user_id"),
                mentioned_key_id: row.get("mentioned_key_id"),
                mentioned_name: row.get("mentioned_name"),
                mention_type: row.get("mention_type"),
            })
            .collect())
    }

    async fn list_issue_mentions(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<std::collections::HashMap<Uuid, Vec<CommentMention>>, StorageError> {
        let rows = sqlx::query(
            "SELECT cm.id, cm.comment_id, cm.mentioned_user_id, cm.mentioned_key_id, cm.mentioned_name, cm.mention_type \
             FROM comment_mentions cm \
             JOIN issue_comments ic ON ic.id = cm.comment_id \
             WHERE cm.repo_id = $1 AND ic.issue_id = $2 \
             ORDER BY cm.created_at ASC",
        )
        .bind(repo_id)
        .bind(issue_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let mut map: std::collections::HashMap<Uuid, Vec<CommentMention>> = std::collections::HashMap::new();
        for row in rows {
            let mention = CommentMention {
                id: row.get("id"),
                comment_id: row.get("comment_id"),
                mentioned_user_id: row.get("mentioned_user_id"),
                mentioned_key_id: row.get("mentioned_key_id"),
                mentioned_name: row.get("mentioned_name"),
                mention_type: row.get("mention_type"),
            };
            map.entry(mention.comment_id).or_default().push(mention);
        }
        Ok(map)
    }
}

// ── IssueLinkStore ────────────────────────────────────────────────────────────

#[async_trait]
impl IssueLinkStore for PostgresStorage {
    async fn create_link(
        &self,
        repo_id: &Uuid,
        source_id: &Uuid,
        link: NewIssueLink,
    ) -> Result<IssueLink, StorageError> {
        sqlx::query(
            r#"
            INSERT INTO issue_links (repo_id, source_id, target_id, relationship)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (source_id, target_id) DO UPDATE SET relationship = $4
            "#,
        )
        .bind(repo_id)
        .bind(source_id)
        .bind(link.target_id)
        .bind(link.relationship.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(IssueLink {
            source_id: *source_id,
            target_id: link.target_id,
            relationship: link.relationship,
        })
    }

    async fn list_links(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueLink>, StorageError> {
        let rows = sqlx::query(
            r#"
            SELECT source_id, target_id, relationship
            FROM issue_links
            WHERE repo_id = $1 AND (source_id = $2 OR target_id = $2)
            "#,
        )
        .bind(repo_id)
        .bind(issue_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let source_id: Uuid = row.get("source_id");
                let target_id: Uuid = row.get("target_id");
                let rel_str: String = row.get("relationship");
                let relationship =
                    IssueLinkRelationship::from_db_str(&rel_str).unwrap_or(IssueLinkRelationship::RelatesTo);
                // Return raw direction so API handlers can apply correct inverse strings.
                IssueLink {
                    source_id,
                    target_id,
                    relationship,
                }
            })
            .collect())
    }

    async fn delete_link(
        &self,
        repo_id: &Uuid,
        source_id: &Uuid,
        target_id: &Uuid,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "DELETE FROM issue_links WHERE repo_id = $1 AND source_id = $2 AND target_id = $3",
        )
        .bind(repo_id)
        .bind(source_id)
        .bind(target_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

// ── AttachmentStore ───────────────────────────────────────────────────────────

#[async_trait]
impl AttachmentStore for PostgresStorage {
    async fn create_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        attachment: NewIssueAttachment,
    ) -> Result<IssueAttachment, StorageError> {
        let row = sqlx::query(
            r#"
            INSERT INTO issue_attachments
                (repo_id, issue_id, filename, content_type, size_bytes, s3_key, uploaded_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id, issue_id, filename, content_type, size_bytes, s3_key, uploaded_by, created_at
            "#,
        )
        .bind(repo_id)
        .bind(issue_id)
        .bind(&attachment.filename)
        .bind(&attachment.content_type)
        .bind(attachment.size_bytes)
        .bind(&attachment.s3_key)
        .bind(&attachment.uploaded_by)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("unique") || msg.contains("duplicate") {
                StorageError::Conflict(format!(
                    "attachment '{}' already exists on this issue",
                    attachment.filename
                ))
            } else {
                StorageError::Database(msg)
            }
        })?;

        Ok(IssueAttachment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            filename: row.get("filename"),
            content_type: row.get("content_type"),
            size_bytes: row.get("size_bytes"),
            s3_key: row.get("s3_key"),
            uploaded_by: row.get("uploaded_by"),
            created_at: row.get("created_at"),
        })
    }

    async fn list_attachments(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueAttachment>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, issue_id, filename, content_type, size_bytes, s3_key, uploaded_by, created_at \
             FROM issue_attachments \
             WHERE repo_id = $1 AND issue_id = $2 \
             ORDER BY created_at ASC",
        )
        .bind(repo_id)
        .bind(issue_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| IssueAttachment {
                id: row.get("id"),
                issue_id: row.get("issue_id"),
                filename: row.get("filename"),
                content_type: row.get("content_type"),
                size_bytes: row.get("size_bytes"),
                s3_key: row.get("s3_key"),
                uploaded_by: row.get("uploaded_by"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn get_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        filename: &str,
    ) -> Result<IssueAttachment, StorageError> {
        let row = sqlx::query(
            "SELECT id, issue_id, filename, content_type, size_bytes, s3_key, uploaded_by, created_at \
             FROM issue_attachments \
             WHERE repo_id = $1 AND issue_id = $2 AND filename = $3",
        )
        .bind(repo_id)
        .bind(issue_id)
        .bind(filename)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("attachment '{filename}' not found")))?;

        Ok(IssueAttachment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            filename: row.get("filename"),
            content_type: row.get("content_type"),
            size_bytes: row.get("size_bytes"),
            s3_key: row.get("s3_key"),
            uploaded_by: row.get("uploaded_by"),
            created_at: row.get("created_at"),
        })
    }

    async fn delete_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        filename: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "DELETE FROM issue_attachments WHERE repo_id = $1 AND issue_id = $2 AND filename = $3",
        )
        .bind(repo_id)
        .bind(issue_id)
        .bind(filename)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}
