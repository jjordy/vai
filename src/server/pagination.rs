//! Server-side pagination types — HTTP query extraction and response envelope.
//!
//! [`PaginationParams`] is extracted from query strings on list endpoints.
//! [`PaginatedResponse<T>`] is the standard JSON response envelope for all
//! paginated list endpoints.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::storage::pagination::ListQuery;

// ── Request types ──────────────────────────────────────────────────────────

/// Query parameters accepted by all paginated list endpoints.
///
/// Used with axum's [`Query`][axum::extract::Query] extractor alongside any
/// endpoint-specific filter params.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PaginationParams {
    /// 1-indexed page number. Defaults to 1.
    pub page: Option<u32>,
    /// Number of items per page. Defaults to 25. Maximum 100.
    pub per_page: Option<u32>,
    /// Comma-separated sort fields, e.g. `created_at:desc,priority:asc`.
    pub sort: Option<String>,
}

// ── Response types ─────────────────────────────────────────────────────────

/// Pagination metadata included in every paginated response.
#[derive(Debug, Serialize, ToSchema)]
pub struct PaginationMeta {
    /// Current page (1-indexed).
    pub page: u32,
    /// Items per page.
    pub per_page: u32,
    /// Total number of matching items across all pages.
    pub total: u64,
    /// Total number of pages.
    pub total_pages: u32,
}

/// Standard JSON envelope for paginated list endpoints.
///
/// ```json
/// {
///   "data": [...],
///   "pagination": { "page": 1, "per_page": 25, "total": 342, "total_pages": 14 }
/// }
/// ```
#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedResponse<T: Serialize> {
    /// The items on the current page.
    pub data: Vec<T>,
    /// Pagination metadata.
    pub pagination: PaginationMeta,
}

impl<T: Serialize> PaginatedResponse<T> {
    /// Construct a [`PaginatedResponse`] from items, the total row count, and
    /// the [`ListQuery`] that was used to fetch the page.
    pub fn new(items: Vec<T>, total: u64, query: &ListQuery) -> Self {
        let per_page = query.per_page;
        let page = query.page;
        let total_pages = if per_page == 0 || per_page == u32::MAX {
            1
        } else {
            ((total + per_page as u64 - 1) / per_page as u64) as u32
        };
        Self {
            data: items,
            pagination: PaginationMeta {
                page,
                per_page,
                total,
                total_pages,
            },
        }
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::pagination::ListQuery;

    #[test]
    fn total_pages_exact_division() {
        let q = ListQuery { page: 1, per_page: 25, sort: vec![] };
        let resp = PaginatedResponse::new(vec![0u32; 25], 100, &q);
        assert_eq!(resp.pagination.total_pages, 4);
    }

    #[test]
    fn total_pages_rounds_up() {
        let q = ListQuery { page: 2, per_page: 25, sort: vec![] };
        let resp = PaginatedResponse::new(vec![0u32; 10], 35, &q);
        assert_eq!(resp.pagination.total_pages, 2);
        assert_eq!(resp.pagination.page, 2);
        assert_eq!(resp.pagination.total, 35);
    }

    #[test]
    fn total_pages_zero_total() {
        let q = ListQuery { page: 1, per_page: 25, sort: vec![] };
        let resp = PaginatedResponse::<u32>::new(vec![], 0, &q);
        assert_eq!(resp.pagination.total_pages, 0);
    }

    #[test]
    fn total_pages_default_query_is_one() {
        // per_page = u32::MAX → single page
        let q = ListQuery::default();
        let resp = PaginatedResponse::new(vec![1u32, 2, 3], 3, &q);
        assert_eq!(resp.pagination.total_pages, 1);
    }
}
