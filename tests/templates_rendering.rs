//! Integration tests for the embedded agent loop templates.
//!
//! Verifies that all 8 template lookups (4 project types × 2 kinds) succeed,
//! are non-empty, and contain expected sentinel phrases.

use vai::cli::agent_loop::{
    templates::{template, TemplateKind},
    ProjectType,
};

#[test]
fn all_eight_templates_are_non_empty() {
    let kinds = [TemplateKind::Prompt, TemplateKind::AgentTomlPartial];
    let types = [
        ProjectType::FrontendReact,
        ProjectType::BackendRust,
        ProjectType::BackendTypescript,
        ProjectType::Generic,
    ];
    for pt in types {
        for kind in kinds {
            let s = template(pt, kind);
            assert!(
                !s.is_empty(),
                "template({pt:?}, {kind:?}) must not be empty"
            );
        }
    }
}

#[test]
fn frontend_react_prompt_has_playwright_section() {
    let s = template(ProjectType::FrontendReact, TemplateKind::Prompt);
    assert!(
        s.contains("Playwright"),
        "frontend-react prompt must contain a Playwright MCP section"
    );
}

#[test]
fn frontend_react_prompt_has_three_phase_workflow() {
    let s = template(ProjectType::FrontendReact, TemplateKind::Prompt);
    assert!(
        (s.contains("EXPLORE") && s.contains("IMPLEMENT") && s.contains("VERIFY"))
            || s.contains("three-phase"),
        "frontend-react prompt must describe a three-phase workflow"
    );
}

#[test]
fn frontend_react_toml_has_pnpm_test_e2e() {
    let s = template(ProjectType::FrontendReact, TemplateKind::AgentTomlPartial);
    assert!(
        s.contains("test:e2e"),
        "frontend-react agent.toml.partial must include test:e2e"
    );
}

#[test]
fn backend_rust_prompt_has_cargo_phases() {
    let s = template(ProjectType::BackendRust, TemplateKind::Prompt);
    assert!(s.contains("cargo"), "backend-rust prompt must reference cargo commands");
    assert!(
        s.contains("EXPLORE") && s.contains("IMPLEMENT") && s.contains("VERIFY"),
        "backend-rust prompt must have three-phase structure"
    );
}

#[test]
fn backend_rust_toml_partial_has_required_cargo_commands() {
    let s = template(ProjectType::BackendRust, TemplateKind::AgentTomlPartial);
    assert!(s.contains("cargo check"), "must include cargo check");
    assert!(s.contains("cargo clippy"), "must include cargo clippy");
    assert!(s.contains("cargo test"), "must include cargo test");
}

#[test]
fn backend_typescript_toml_partial_has_tsc_and_test() {
    let s = template(ProjectType::BackendTypescript, TemplateKind::AgentTomlPartial);
    assert!(s.contains("tsc"), "must include tsc step");
    assert!(s.contains("pnpm test"), "must include pnpm test");
}

#[test]
fn generic_prompt_has_loop_instructions() {
    let s = template(ProjectType::Generic, TemplateKind::Prompt);
    assert!(
        s.contains("vai agent claim"),
        "generic prompt must mention vai agent claim"
    );
}

#[test]
fn generic_toml_partial_has_placeholder_comment() {
    let s = template(ProjectType::Generic, TemplateKind::AgentTomlPartial);
    assert!(
        s.contains("Add your quality-check commands"),
        "generic partial must have placeholder comment"
    );
}

#[test]
fn all_prompts_contain_repo_name_token() {
    for pt in [
        ProjectType::FrontendReact,
        ProjectType::BackendRust,
        ProjectType::BackendTypescript,
        ProjectType::Generic,
    ] {
        let s = template(pt, TemplateKind::Prompt);
        assert!(
            s.contains("{{REPO_NAME}}"),
            "{pt:?} prompt must contain {{{{REPO_NAME}}}} token"
        );
    }
}
