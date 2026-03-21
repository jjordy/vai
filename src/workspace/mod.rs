//! Workspace management — isolated environments for agent changes.
//!
//! A workspace is an isolated environment where an agent makes changes against
//! a snapshot of the codebase. Changes are tracked as events and can be
//! submitted for merging or discarded.
