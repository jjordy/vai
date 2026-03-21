//! Semantic merge engine — three-level merge analysis and conflict resolution.
//!
//! The merge engine integrates workspace changes back into the main version.
//! Unlike git's line-based merge, vai operates at three levels: textual,
//! structural (AST), and referential (semantic graph edges).
