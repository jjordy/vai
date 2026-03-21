//! vai — version control for AI agents
//!
//! Core library providing the semantic graph, event log, workspace management,
//! merge engine, and version history for AI-native version control.

pub mod auth;
pub mod cli;
pub mod clone;
pub mod conflict;
pub mod diff;
pub mod event_log;
pub mod graph;
pub mod issue;
pub mod merge;
pub mod remote_workspace;
pub mod repo;
pub mod server;
pub mod sync;
pub mod version;
pub mod workspace;
