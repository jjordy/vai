//! Integration tests for TypeScript/TSX entity and relationship extraction.
//!
//! Verifies that the semantic graph engine correctly parses representative
//! TypeScript/React patterns: classes, interfaces, type aliases, components,
//! hooks, import/export statements, and mixed-language repos.

use vai::graph::{EntityKind, RelationshipKind, parse_source_file, parse_typescript_source};

// ── Sample TypeScript source files ─────────────────────────────────────────

/// A TypeScript file with a class hierarchy, interface, and type alias.
const CLASSES_TS: &str = r#"
interface Animal {
    name: string;
    speak(): void;
}

type AnimalKind = "dog" | "cat" | "bird";

class BaseAnimal implements Animal {
    name: string;

    constructor(name: string) {
        this.name = name;
    }

    speak(): void {
        console.log("...");
    }

    describe(): string {
        return `I am ${this.name}`;
    }
}

class Dog extends BaseAnimal {
    breed: string;

    constructor(name: string, breed: string) {
        super(name);
        this.breed = breed;
    }

    speak(): void {
        console.log("Woof!");
    }
}
"#;

/// A TSX file with React components and custom hooks.
const COMPONENTS_TSX: &str = r#"
import React, { useState, useEffect } from "react";

type ButtonProps = {
    label: string;
    onClick: () => void;
};

function useCounter(initial: number) {
    const [count, setCount] = useState(initial);
    useEffect(() => {
        document.title = `Count: ${count}`;
    }, [count]);
    return { count, setCount };
}

const useTheme = () => {
    const [theme, setTheme] = useState("light");
    return { theme, setTheme };
};

function Button({ label, onClick }: ButtonProps) {
    return <button onClick={onClick}>{label}</button>;
}

const Counter = ({ initial }: { initial: number }) => {
    const { count, setCount } = useCounter(initial);
    return (
        <div>
            <p>{count}</p>
            <Button label="Increment" onClick={() => setCount(count + 1)} />
        </div>
    );
};

export default Counter;
"#;

/// A TypeScript file focused on imports and exports.
const EXPORTS_TS: &str = r#"
import { EventEmitter } from "events";
import type { Serializable } from "./types";

export interface Config {
    host: string;
    port: number;
}

export type ConnectionStatus = "connected" | "disconnected" | "error";

export enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

export function createConfig(host: string, port: number): Config {
    return { host, port };
}

export class Connection extends EventEmitter {
    private status: ConnectionStatus = "disconnected";

    connect(): void {
        this.status = "connected";
        this.emit("connect");
    }

    disconnect(): void {
        this.status = "disconnected";
        this.emit("disconnect");
    }
}
"#;

// ── Helpers ─────────────────────────────────────────────────────────────────

fn count_kind(entities: &[vai::graph::Entity], kind: EntityKind) -> usize {
    entities.iter().filter(|e| e.kind == kind).count()
}

fn has_entity(entities: &[vai::graph::Entity], kind: EntityKind, name: &str) -> bool {
    entities.iter().any(|e| e.kind == kind && e.name == name)
}

fn has_relationship(
    rels: &[vai::graph::Relationship],
    kind: RelationshipKind,
    from_name: &str,
    to_name: &str,
    entities: &[vai::graph::Entity],
) -> bool {
    let from_id = entities.iter().find(|e| e.name == from_name).map(|e| &e.id);
    let to_id = entities.iter().find(|e| e.name == to_name).map(|e| &e.id);
    match (from_id, to_id) {
        (Some(from), Some(to)) => rels
            .iter()
            .any(|r| r.kind == kind && &r.from_entity == from && &r.to_entity == to),
        _ => false,
    }
}

// ── Tests: class hierarchy ────────────────────────────────────────────────────

#[test]
fn test_ts_class_extraction() {
    let (entities, _) = parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Class, "BaseAnimal"), "BaseAnimal class missing");
    assert!(has_entity(&entities, EntityKind::Class, "Dog"), "Dog class missing");
    assert_eq!(count_kind(&entities, EntityKind::Class), 2, "expected 2 classes");
}

#[test]
fn test_ts_interface_extraction() {
    let (entities, _) = parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Interface, "Animal"), "Animal interface missing");
    assert_eq!(count_kind(&entities, EntityKind::Interface), 1, "expected 1 interface");
}

#[test]
fn test_ts_type_alias_extraction() {
    let (entities, _) = parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::TypeAlias, "AnimalKind"), "AnimalKind type alias missing");
    assert_eq!(count_kind(&entities, EntityKind::TypeAlias), 1, "expected 1 type alias");
}

#[test]
fn test_ts_method_extraction() {
    let (entities, _) = parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();

    // BaseAnimal has: constructor, speak, describe; Dog has: constructor, speak
    let method_count = count_kind(&entities, EntityKind::Method);
    assert!(method_count >= 4, "expected at least 4 methods, got {method_count}");
    assert!(has_entity(&entities, EntityKind::Method, "describe"), "describe method missing");
    assert!(has_entity(&entities, EntityKind::Method, "speak"), "speak method missing");
}

#[test]
fn test_ts_class_contains_methods() {
    let (entities, relationships) =
        parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();

    // BaseAnimal should contain `describe`
    assert!(
        has_relationship(
            &relationships,
            RelationshipKind::Contains,
            "BaseAnimal",
            "describe",
            &entities,
        ),
        "BaseAnimal should contain describe"
    );
}

#[test]
fn test_ts_implements_relationship() {
    let (entities, relationships) =
        parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();

    // BaseAnimal implements Animal → Implements relationship
    let base = entities.iter().find(|e| e.name == "BaseAnimal").expect("BaseAnimal missing");
    let implements_rels: Vec<_> = relationships
        .iter()
        .filter(|r| r.kind == RelationshipKind::Implements && r.from_entity == base.id)
        .collect();
    assert!(!implements_rels.is_empty(), "BaseAnimal should have an Implements relationship");
}

#[test]
fn test_ts_extends_relationship() {
    let (entities, relationships) =
        parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();

    // Dog extends BaseAnimal → Extends relationship
    let dog = entities.iter().find(|e| e.name == "Dog").expect("Dog missing");
    let extends_rels: Vec<_> = relationships
        .iter()
        .filter(|r| r.kind == RelationshipKind::Extends && r.from_entity == dog.id)
        .collect();
    assert!(!extends_rels.is_empty(), "Dog should have an Extends relationship");
}

// ── Tests: React components and hooks ────────────────────────────────────────

#[test]
fn test_tsx_component_extraction() {
    let (entities, _) = parse_source_file("src/Counter.tsx", COMPONENTS_TSX.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Component, "Button"), "Button component missing");
    assert!(has_entity(&entities, EntityKind::Component, "Counter"), "Counter component missing");
    assert_eq!(count_kind(&entities, EntityKind::Component), 2, "expected 2 components");
}

#[test]
fn test_tsx_hook_extraction() {
    let (entities, _) = parse_source_file("src/Counter.tsx", COMPONENTS_TSX.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Hook, "useCounter"), "useCounter hook missing");
    assert!(has_entity(&entities, EntityKind::Hook, "useTheme"), "useTheme hook missing");
    assert_eq!(count_kind(&entities, EntityKind::Hook), 2, "expected 2 hooks");
}

#[test]
fn test_tsx_type_alias_extraction() {
    let (entities, _) = parse_source_file("src/Counter.tsx", COMPONENTS_TSX.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::TypeAlias, "ButtonProps"), "ButtonProps type alias missing");
}

#[test]
fn test_tsx_import_extraction() {
    let (entities, _) = parse_source_file("src/Counter.tsx", COMPONENTS_TSX.as_bytes()).unwrap();

    // Should extract the import statement from "react"
    let use_stmts = count_kind(&entities, EntityKind::UseStatement);
    assert!(use_stmts >= 1, "expected at least 1 import/use statement, got {use_stmts}");
}

// ── Tests: exports ────────────────────────────────────────────────────────────

#[test]
fn test_ts_exported_interface() {
    let (entities, _) = parse_source_file("src/connection.ts", EXPORTS_TS.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Interface, "Config"), "Config interface missing");
}

#[test]
fn test_ts_exported_enum() {
    let (entities, _) = parse_source_file("src/connection.ts", EXPORTS_TS.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Enum, "LogLevel"), "LogLevel enum missing");
    assert_eq!(count_kind(&entities, EntityKind::Enum), 1, "expected 1 enum");
}

#[test]
fn test_ts_exported_function() {
    let (entities, _) = parse_source_file("src/connection.ts", EXPORTS_TS.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Function, "createConfig"), "createConfig function missing");
}

#[test]
fn test_ts_exported_class() {
    let (entities, _) = parse_source_file("src/connection.ts", EXPORTS_TS.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Class, "Connection"), "Connection class missing");
}

#[test]
fn test_ts_exported_class_methods() {
    let (entities, _) = parse_source_file("src/connection.ts", EXPORTS_TS.as_bytes()).unwrap();

    assert!(has_entity(&entities, EntityKind::Method, "connect"), "connect method missing");
    assert!(has_entity(&entities, EntityKind::Method, "disconnect"), "disconnect method missing");
}

#[test]
fn test_ts_exported_class_methods_contained() {
    let (entities, relationships) =
        parse_source_file("src/connection.ts", EXPORTS_TS.as_bytes()).unwrap();

    assert!(
        has_relationship(
            &relationships,
            RelationshipKind::Contains,
            "Connection",
            "connect",
            &entities,
        ),
        "Connection should contain connect"
    );
}

#[test]
fn test_ts_export_statements_recorded() {
    let (entities, _) = parse_source_file("src/connection.ts", EXPORTS_TS.as_bytes()).unwrap();

    // Multiple top-level export statements should be recorded.
    let export_count = count_kind(&entities, EntityKind::ExportStatement);
    assert!(export_count >= 1, "expected at least 1 ExportStatement, got {export_count}");
}

// ── Tests: extension dispatch ─────────────────────────────────────────────────

#[test]
fn test_parse_ts_extension() {
    // .ts files should be parsed as TypeScript (not empty)
    let (entities, _) = parse_source_file("src/foo.ts", CLASSES_TS.as_bytes()).unwrap();
    assert!(!entities.is_empty(), ".ts files should yield entities");
}

#[test]
fn test_parse_tsx_extension() {
    // .tsx files should be parsed as TSX
    let (entities, _) = parse_source_file("src/App.tsx", COMPONENTS_TSX.as_bytes()).unwrap();
    assert!(!entities.is_empty(), ".tsx files should yield entities");
}

#[test]
fn test_parse_js_extension() {
    // .js files use the TypeScript grammar
    let simple_js = b"function hello() { return 42; }";
    let (entities, _) = parse_source_file("src/util.js", simple_js).unwrap();
    assert!(!entities.is_empty(), ".js files should yield entities");
}

#[test]
fn test_parse_jsx_extension() {
    // .jsx files use the TSX grammar
    let simple_jsx = b"const App = () => <div>Hello</div>;";
    let (entities, _) = parse_source_file("src/App.jsx", simple_jsx).unwrap();
    assert!(!entities.is_empty(), ".jsx files should yield entities");
}

#[test]
fn test_unknown_extension_returns_empty() {
    let (entities, rels) = parse_source_file("src/style.css", b"body { color: red; }").unwrap();
    assert!(entities.is_empty(), "unknown extensions should return empty entities");
    assert!(rels.is_empty(), "unknown extensions should return empty relationships");
}

// ── Tests: mixed-language repo ───────────────────────────────────────────────

#[test]
fn test_mixed_language_rs_and_ts() {
    use vai::graph::GraphSnapshot;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("snapshot.db");
    let snap = GraphSnapshot::open(&db_path).unwrap();

    // Rust file
    let rust_src = br#"
pub struct Config { pub name: String }
impl Config {
    pub fn new(name: &str) -> Self { Config { name: name.to_string() } }
}
"#;
    snap.update_file("src/config.rs", rust_src).unwrap();

    // TypeScript file
    snap.update_file("src/config.ts", CLASSES_TS.as_bytes()).unwrap();

    // TSX file
    snap.update_file("src/Counter.tsx", COMPONENTS_TSX.as_bytes()).unwrap();

    let all = snap.all_entities().unwrap();

    // Rust entities
    assert!(
        all.iter().any(|e| e.kind == EntityKind::Struct && e.name == "Config"),
        "Config struct from Rust missing"
    );
    assert!(
        all.iter().any(|e| e.kind == EntityKind::Method && e.name == "new"),
        "new method from Rust missing"
    );

    // TypeScript entities
    assert!(
        all.iter().any(|e| e.kind == EntityKind::Class && e.name == "BaseAnimal"),
        "BaseAnimal class from TS missing"
    );
    assert!(
        all.iter().any(|e| e.kind == EntityKind::Interface && e.name == "Animal"),
        "Animal interface from TS missing"
    );

    // TSX entities
    assert!(
        all.iter().any(|e| e.kind == EntityKind::Component && e.name == "Counter"),
        "Counter component from TSX missing"
    );
    assert!(
        all.iter().any(|e| e.kind == EntityKind::Hook && e.name == "useCounter"),
        "useCounter hook from TSX missing"
    );

    // Graph should have entities from all three files
    let rs_count = all.iter().filter(|e| e.file_path == "src/config.rs").count();
    let ts_count = all.iter().filter(|e| e.file_path == "src/config.ts").count();
    let tsx_count = all.iter().filter(|e| e.file_path == "src/Counter.tsx").count();
    assert!(rs_count > 0, "no entities from .rs file");
    assert!(ts_count > 0, "no entities from .ts file");
    assert!(tsx_count > 0, "no entities from .tsx file");
}

#[test]
fn test_mixed_language_entity_counts() {
    use vai::graph::GraphSnapshot;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("snapshot.db");
    let snap = GraphSnapshot::open(&db_path).unwrap();

    snap.update_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();
    snap.update_file("src/Counter.tsx", COMPONENTS_TSX.as_bytes()).unwrap();
    snap.update_file("src/connection.ts", EXPORTS_TS.as_bytes()).unwrap();

    let stats = snap.stats().unwrap();

    assert!(stats.entity_count >= 10, "expected many entities across 3 files, got {}", stats.entity_count);
    assert!(stats.file_count == 3, "expected 3 files in graph, got {}", stats.file_count);
}

// ── Tests: stable entity IDs ─────────────────────────────────────────────────

#[test]
fn test_entity_ids_are_stable() {
    // Parsing the same source twice should produce identical entity IDs.
    let (entities1, _) = parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();
    let (entities2, _) = parse_source_file("src/animals.ts", CLASSES_TS.as_bytes()).unwrap();

    let ids1: std::collections::HashSet<_> = entities1.iter().map(|e| &e.id).collect();
    let ids2: std::collections::HashSet<_> = entities2.iter().map(|e| &e.id).collect();
    assert_eq!(ids1, ids2, "entity IDs should be stable across re-parses");
}

#[test]
fn test_entity_ids_differ_by_file_path() {
    // Same source parsed with a different file path should produce different IDs.
    let (entities_a, _) = parse_source_file("src/a.ts", CLASSES_TS.as_bytes()).unwrap();
    let (entities_b, _) = parse_source_file("src/b.ts", CLASSES_TS.as_bytes()).unwrap();

    let ids_a: std::collections::HashSet<_> = entities_a.iter().map(|e| &e.id).collect();
    let ids_b: std::collections::HashSet<_> = entities_b.iter().map(|e| &e.id).collect();
    assert!(
        ids_a.is_disjoint(&ids_b),
        "entities from different files should have different IDs"
    );
}

// ── Tests: direct parse_typescript_source API ─────────────────────────────────

#[test]
fn test_parse_typescript_source_non_tsx() {
    let (entities, _) = parse_typescript_source("src/foo.ts", CLASSES_TS.as_bytes(), false).unwrap();
    assert!(!entities.is_empty(), "parse_typescript_source should extract entities");
    assert!(has_entity(&entities, EntityKind::Class, "Dog"), "Dog class should be extracted");
}

#[test]
fn test_parse_typescript_source_tsx_mode() {
    let (entities, _) =
        parse_typescript_source("src/App.tsx", COMPONENTS_TSX.as_bytes(), true).unwrap();
    assert!(has_entity(&entities, EntityKind::Component, "Counter"), "Counter component expected in TSX mode");
}
