# Phase 7: TypeScript and React Language Support

## Summary

Add TypeScript and TSX/JSX parsing to the semantic graph engine via tree-sitter. This enables entity-level diff, merge, scope inference, and conflict detection for TypeScript/React projects — the same capabilities currently available for Rust.

## Motivation

The vai dashboard (and most web projects vai will manage) is written in TypeScript with React. Without language support, vai operates at file-level only — it can detect that two agents modified the same file, but not that they modified the same function or component. This degrades the merge engine, conflict detection, and scope inference to the point where vai offers little advantage over git for non-Rust codebases.

## Requirements

### 7.1: Tree-sitter TypeScript Grammar

Add `tree-sitter-typescript` as a dependency (already approved in CLAUDE.md). Wire it into the graph engine's parser dispatch so that `.ts`, `.tsx`, `.js`, and `.jsx` files are parsed alongside `.rs` files.

The parser should select the grammar based on file extension:
- `.rs` → tree-sitter-rust (existing)
- `.ts` → tree-sitter-typescript
- `.tsx` → tree-sitter-tsx
- `.js` → tree-sitter-typescript (JS is a subset)
- `.jsx` → tree-sitter-tsx

### 7.2: TypeScript Entity Extraction

Extract the following entity types from TypeScript/TSX ASTs:

| Entity Kind | AST Node Types |
|-------------|---------------|
| Function | `function_declaration`, `arrow_function` (when assigned to a variable) |
| Method | `method_definition` inside class body |
| Class | `class_declaration` |
| Interface | `interface_declaration` |
| TypeAlias | `type_alias_declaration` |
| Enum | `enum_declaration` |
| Component | `arrow_function` or `function_declaration` that returns JSX (heuristic: body contains `jsx_element` or `jsx_self_closing_element`) |
| Hook | Functions starting with `use` (convention-based) |
| Module | Not applicable (TypeScript uses file-level modules) |
| ExportStatement | `export_statement` (for tracking public API surface) |

Each entity gets:
- `id`: SHA-256 of `{file_path}::{qualified_name}`
- `kind`: from the table above
- `name`: identifier name
- `qualified_name`: for nested entities, `ClassName::methodName` or `ComponentName`
- `file_path`: relative path
- `byte_range` and `line_range`: source location
- `parent_entity`: ID of containing class/component if nested

### 7.3: TypeScript Relationship Extraction

Extract relationships between entities:

| Relationship | Detection |
|-------------|-----------|
| Contains | Class contains methods, component contains hooks |
| Calls | Function/method body contains `call_expression` referencing another entity |
| Imports | `import_statement` referencing a known entity |
| Implements | `class_declaration` with `implements` clause |
| Extends | `class_declaration` with `extends` clause |

### 7.4: File Collection Update

Update `collect_source_files` in `repo.rs` to include `.ts`, `.tsx`, `.js`, and `.jsx` files alongside `.rs`. The `vai.toml` ignore patterns should still apply.

### 7.5: Graph Refresh and Init Update

Ensure `vai init` and `vai graph refresh` parse TypeScript files. The graph snapshot should contain entities from all supported languages. Entity IDs are stable across languages since they're based on `{file_path}::{qualified_name}`.

## Out of Scope

- Python support (future PRD)
- Cross-language relationships (e.g., a Rust WASM module called from TypeScript)
- Type-checking or type-aware analysis (we parse structure, not types)
- Decorators / metadata extraction
- Module resolution (we don't follow `import` paths across files)

## Issues

1. **Add tree-sitter-typescript dependency and grammar loading** — Add `tree-sitter-typescript` crate, wire up grammar selection by file extension (`.ts`, `.tsx`, `.js`, `.jsx`). Extend the parser dispatch in `graph/mod.rs`. Priority: high.

2. **Implement TypeScript entity extraction** — Walk the TypeScript/TSX AST and extract functions, classes, interfaces, type aliases, enums, methods, components (JSX-returning functions), hooks (use-prefixed functions), and export statements. Map to the existing `Entity` struct with appropriate `EntityKind` variants. Priority: high.

3. **Implement TypeScript relationship extraction** — Detect Contains (class→method, component→hook), Calls (call expressions), Imports (import statements referencing known entities), Implements, and Extends relationships. Priority: high.

4. **Update file collection to include TypeScript files** — Extend `collect_source_files` and `collect_recursive` in `repo.rs` to match `.ts`, `.tsx`, `.js`, `.jsx` extensions alongside `.rs`. Priority: medium.

5. **Add TypeScript parsing integration tests** — Create test files with representative TypeScript/React patterns (classes, interfaces, components, hooks, imports) and verify entity and relationship extraction. Test mixed-language repos (Rust + TypeScript). Priority: medium.

6. **Add new EntityKind variants for TypeScript** — Extend the `EntityKind` enum with `Class`, `Interface`, `TypeAlias`, `Component`, `Hook`, `ExportStatement`. Update serialization, display, and the snapshot DB schema. Priority: high.
