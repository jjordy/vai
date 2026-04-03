//! Shared pagination infrastructure for storage layer list queries.
//!
//! This module provides [`ListQuery`] (pagination + sort parameters) and
//! [`ListResult<T>`] (paginated result with total count) used by all list
//! trait methods.

use std::collections::HashMap;

// ── Error type ─────────────────────────────────────────────────────────────

/// Errors returned when parsing or validating pagination/sort parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaginationError {
    /// `page` must be ≥ 1.
    InvalidPage(u32),
    /// `per_page` must be between 1 and 100.
    InvalidPerPage(u32),
    /// Sort column is not in the endpoint's allowlist.
    UnknownSortColumn(String),
    /// Sort entry could not be parsed (expected `column` or `column:direction`).
    InvalidSortFormat(String),
}

impl std::fmt::Display for PaginationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPage(p) => write!(f, "invalid page {p}: must be ≥ 1"),
            Self::InvalidPerPage(n) => write!(f, "invalid per_page {n}: must be 1–100"),
            Self::UnknownSortColumn(c) => write!(f, "unknown sort column '{c}'"),
            Self::InvalidSortFormat(s) => {
                write!(f, "invalid sort format '{s}': expected 'column' or 'column:asc|desc'")
            }
        }
    }
}

impl std::error::Error for PaginationError {}

// ── Sort types ─────────────────────────────────────────────────────────────

/// Direction for a single sort field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortDirection {
    /// Ascending order (default when direction is omitted).
    Asc,
    /// Descending order.
    Desc,
}

impl SortDirection {
    /// Returns the SQL keyword for this direction.
    pub fn sql(&self) -> &'static str {
        match self {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        }
    }
}

/// A single column + direction pair in an `ORDER BY` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortField {
    /// API-level column name (e.g. `"created_at"`).
    pub column: String,
    /// Sort direction.
    pub direction: SortDirection,
}

// ── ListQuery ──────────────────────────────────────────────────────────────

/// Pagination and sort parameters passed to storage list methods.
///
/// Use [`ListQuery::from_params`] to construct a validated instance from raw
/// query string values. Use [`ListQuery::default`] for call sites that do not
/// need pagination (returns all rows, page 1, `per_page = u32::MAX`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery {
    /// 1-indexed page number. Default: 1.
    pub page: u32,
    /// Rows per page. Default: 25. Max: 100. Use [`u32::MAX`] for "all rows".
    pub per_page: u32,
    /// Ordered list of sort fields. Empty means the storage layer uses its
    /// own default ordering.
    pub sort: Vec<SortField>,
}

impl Default for ListQuery {
    /// Returns a query that fetches all rows (backward-compatible default for
    /// callers that have not yet been updated to pass explicit pagination).
    fn default() -> Self {
        Self {
            page: 1,
            per_page: u32::MAX,
            sort: vec![],
        }
    }
}

impl ListQuery {
    /// Parse and validate raw query-string pagination parameters.
    ///
    /// * `page` — raw page number from request (default: 1)
    /// * `per_page` — raw per-page count from request (default: 25, max: 100)
    /// * `sort_str` — optional `?sort=` string, e.g. `"created_at:desc,priority:asc"`
    /// * `allowed_columns` — set of API column names the endpoint permits for sorting
    ///
    /// Returns a [`PaginationError`] (suitable for a 400 response) if any
    /// parameter is invalid.
    pub fn from_params(
        page: Option<u32>,
        per_page: Option<u32>,
        sort_str: Option<&str>,
        allowed_columns: &[&str],
    ) -> Result<Self, PaginationError> {
        let page = page.unwrap_or(1);
        if page == 0 {
            return Err(PaginationError::InvalidPage(page));
        }

        let per_page = per_page.unwrap_or(25);
        if per_page == 0 || per_page > 100 {
            return Err(PaginationError::InvalidPerPage(per_page));
        }

        let sort = parse_sort(sort_str.unwrap_or(""), allowed_columns)?;

        Ok(Self { page, per_page, sort })
    }

    /// Generate a SQL `ORDER BY` clause from the sort fields, mapping API
    /// column names to actual DB column names via `column_map`.
    ///
    /// `column_map` keys are API names; values are DB column names.
    /// Returns an empty string when `self.sort` is empty (the caller should
    /// supply its own default ordering in that case).
    ///
    /// # Example
    ///
    /// ```rust
    /// # use std::collections::HashMap;
    /// # use vai::storage::pagination::{ListQuery, SortField, SortDirection};
    /// let q = ListQuery {
    ///     page: 1, per_page: 25,
    ///     sort: vec![SortField { column: "created_at".into(), direction: SortDirection::Desc }],
    /// };
    /// let map: HashMap<&str, &str> = [("created_at", "created_at")].iter().cloned().collect();
    /// assert_eq!(q.sql_order_by(&map), "ORDER BY created_at DESC");
    /// ```
    pub fn sql_order_by(&self, column_map: &HashMap<&str, &str>) -> String {
        if self.sort.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = self
            .sort
            .iter()
            .filter_map(|sf| {
                column_map.get(sf.column.as_str()).map(|db_col| {
                    format!("{} {}", db_col, sf.direction.sql())
                })
            })
            .collect();
        if parts.is_empty() {
            String::new()
        } else {
            format!("ORDER BY {}", parts.join(", "))
        }
    }

    /// Returns `(limit, offset)` values for a `LIMIT ? OFFSET ?` clause.
    ///
    /// Both are `i64` for compatibility with SQLx bound parameters.
    pub fn sql_limit_offset(&self) -> (i64, i64) {
        let limit = if self.per_page == u32::MAX {
            i64::MAX
        } else {
            self.per_page as i64
        };
        let offset = ((self.page - 1) as i64) * (self.per_page as i64).min(i64::MAX / 2);
        (limit, offset)
    }
}

// ── ListResult ─────────────────────────────────────────────────────────────

/// Result of a paginated list query.
#[derive(Debug, Clone)]
pub struct ListResult<T> {
    /// The items on the current page.
    pub items: Vec<T>,
    /// Total number of matching rows (ignoring LIMIT/OFFSET), used to compute
    /// `total_pages` in the response envelope.
    pub total: u64,
}

// ── Internal helpers ───────────────────────────────────────────────────────

fn parse_sort(sort_str: &str, allowed_columns: &[&str]) -> Result<Vec<SortField>, PaginationError> {
    if sort_str.is_empty() {
        return Ok(vec![]);
    }
    let mut fields = vec![];
    for part in sort_str.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (col, dir) = match part.split_once(':') {
            Some((c, d)) => {
                let direction = match d.to_lowercase().as_str() {
                    "asc" => SortDirection::Asc,
                    "desc" => SortDirection::Desc,
                    _ => return Err(PaginationError::InvalidSortFormat(part.to_string())),
                };
                (c.trim(), direction)
            }
            None => (part, SortDirection::Asc),
        };
        if !allowed_columns.contains(&col) {
            return Err(PaginationError::UnknownSortColumn(col.to_string()));
        }
        fields.push(SortField {
            column: col.to_string(),
            direction: dir,
        });
    }
    Ok(fields)
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOWED: &[&str] = &["created_at", "updated_at", "priority", "status", "title"];

    #[test]
    fn default_returns_all_rows() {
        let q = ListQuery::default();
        assert_eq!(q.page, 1);
        assert_eq!(q.per_page, u32::MAX);
        assert!(q.sort.is_empty());
        let (limit, offset) = q.sql_limit_offset();
        assert_eq!(limit, i64::MAX);
        assert_eq!(offset, 0);
    }

    #[test]
    fn from_params_defaults() {
        let q = ListQuery::from_params(None, None, None, ALLOWED).unwrap();
        assert_eq!(q.page, 1);
        assert_eq!(q.per_page, 25);
        assert!(q.sort.is_empty());
    }

    #[test]
    fn from_params_explicit() {
        let q = ListQuery::from_params(Some(3), Some(10), None, ALLOWED).unwrap();
        assert_eq!(q.page, 3);
        assert_eq!(q.per_page, 10);
        let (limit, offset) = q.sql_limit_offset();
        assert_eq!(limit, 10);
        assert_eq!(offset, 20);
    }

    #[test]
    fn single_sort_desc() {
        let q = ListQuery::from_params(None, None, Some("created_at:desc"), ALLOWED).unwrap();
        assert_eq!(q.sort.len(), 1);
        assert_eq!(q.sort[0].column, "created_at");
        assert_eq!(q.sort[0].direction, SortDirection::Desc);
    }

    #[test]
    fn single_sort_no_direction_defaults_asc() {
        let q = ListQuery::from_params(None, None, Some("priority"), ALLOWED).unwrap();
        assert_eq!(q.sort[0].direction, SortDirection::Asc);
    }

    #[test]
    fn multi_sort() {
        let q =
            ListQuery::from_params(None, None, Some("created_at:desc,priority:asc"), ALLOWED)
                .unwrap();
        assert_eq!(q.sort.len(), 2);
        assert_eq!(q.sort[0].column, "created_at");
        assert_eq!(q.sort[1].column, "priority");
    }

    #[test]
    fn empty_sort_string() {
        let q = ListQuery::from_params(None, None, Some(""), ALLOWED).unwrap();
        assert!(q.sort.is_empty());
    }

    #[test]
    fn unknown_column_returns_error() {
        let err =
            ListQuery::from_params(None, None, Some("nonexistent:asc"), ALLOWED).unwrap_err();
        assert!(matches!(err, PaginationError::UnknownSortColumn(_)));
    }

    #[test]
    fn invalid_direction_returns_error() {
        let err =
            ListQuery::from_params(None, None, Some("created_at:sideways"), ALLOWED).unwrap_err();
        assert!(matches!(err, PaginationError::InvalidSortFormat(_)));
    }

    #[test]
    fn page_zero_returns_error() {
        let err = ListQuery::from_params(Some(0), None, None, ALLOWED).unwrap_err();
        assert!(matches!(err, PaginationError::InvalidPage(0)));
    }

    #[test]
    fn per_page_over_100_returns_error() {
        let err = ListQuery::from_params(None, Some(101), None, ALLOWED).unwrap_err();
        assert!(matches!(err, PaginationError::InvalidPerPage(101)));
    }

    #[test]
    fn per_page_zero_returns_error() {
        let err = ListQuery::from_params(None, Some(0), None, ALLOWED).unwrap_err();
        assert!(matches!(err, PaginationError::InvalidPerPage(0)));
    }

    #[test]
    fn sql_order_by_single() {
        let q = ListQuery {
            page: 1,
            per_page: 25,
            sort: vec![SortField {
                column: "created_at".into(),
                direction: SortDirection::Desc,
            }],
        };
        let map: HashMap<&str, &str> = [("created_at", "i.created_at")].iter().cloned().collect();
        assert_eq!(q.sql_order_by(&map), "ORDER BY i.created_at DESC");
    }

    #[test]
    fn sql_order_by_multi() {
        let q = ListQuery {
            page: 1,
            per_page: 25,
            sort: vec![
                SortField {
                    column: "priority".into(),
                    direction: SortDirection::Asc,
                },
                SortField {
                    column: "created_at".into(),
                    direction: SortDirection::Desc,
                },
            ],
        };
        let map: HashMap<&str, &str> = [("priority", "priority"), ("created_at", "created_at")]
            .iter()
            .cloned()
            .collect();
        assert_eq!(
            q.sql_order_by(&map),
            "ORDER BY priority ASC, created_at DESC"
        );
    }

    #[test]
    fn sql_order_by_empty_sort() {
        let q = ListQuery::default();
        let map: HashMap<&str, &str> = HashMap::new();
        assert_eq!(q.sql_order_by(&map), "");
    }
}
