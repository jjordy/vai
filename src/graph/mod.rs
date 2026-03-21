//! Semantic graph engine — entity extraction and relationship tracking.
//!
//! Parses source files using tree-sitter and represents the codebase as a graph
//! of language-level entities (functions, structs, traits, modules) and their
//! relationships (calls, imports, contains).
