//! vai — version control for AI agents
//!
//! Core library providing the semantic graph, event log, workspace management,
//! merge engine, and version history for AI-native version control.

pub mod auth;
pub mod cli;
pub mod storage;
pub mod clone;
pub mod dashboard;
pub mod escalation;
pub mod conflict;
pub mod diff;
pub mod event_log;
pub mod graph;
pub mod issue;
pub mod merge;
pub mod merge_patterns;
pub mod migration;
pub mod remote_client;
pub mod remote_workspace;
pub mod repo;
pub mod scope_history;
pub mod scope_inference;
pub mod server;
pub mod sync;
pub mod version;
pub mod watcher;
pub mod work_queue;
pub mod workspace;
