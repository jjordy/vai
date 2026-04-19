//! Embedded prompt and `agent.toml` partial templates for `vai agent loop init`.
//!
//! All templates are baked into the binary at compile time via `include_str!`.
//! The init command uses [`template`] to retrieve the correct template string,
//! then substitutes `{{REPO_NAME}}`, `{{SERVER_URL}}`, and `{{AGENT_NAME}}`
//! tokens before writing the files to disk.

use super::detection::ProjectType;

/// Which template file to retrieve for a given project type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    /// The agent prompt written to `.vai/prompt.md`.
    Prompt,
    /// The `[checks]` TOML snippet appended to `agent.toml`.
    AgentTomlPartial,
}

/// Return the static template string for the given `(project_type, kind)` pair.
///
/// All strings are `'static` — they are embedded in the binary by `include_str!`
/// and require no allocation.
pub fn template(project_type: ProjectType, kind: TemplateKind) -> &'static str {
    match (project_type, kind) {
        (ProjectType::FrontendReact, TemplateKind::Prompt) => {
            include_str!("templates/frontend-react/prompt.md")
        }
        (ProjectType::FrontendReact, TemplateKind::AgentTomlPartial) => {
            include_str!("templates/frontend-react/agent.toml.partial")
        }
        (ProjectType::BackendRust, TemplateKind::Prompt) => {
            include_str!("templates/backend-rust/prompt.md")
        }
        (ProjectType::BackendRust, TemplateKind::AgentTomlPartial) => {
            include_str!("templates/backend-rust/agent.toml.partial")
        }
        (ProjectType::BackendTypescript, TemplateKind::Prompt) => {
            include_str!("templates/backend-typescript/prompt.md")
        }
        (ProjectType::BackendTypescript, TemplateKind::AgentTomlPartial) => {
            include_str!("templates/backend-typescript/agent.toml.partial")
        }
        (ProjectType::Generic, TemplateKind::Prompt) => {
            include_str!("templates/generic/prompt.md")
        }
        (ProjectType::Generic, TemplateKind::AgentTomlPartial) => {
            include_str!("templates/generic/agent.toml.partial")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_templates_are_non_empty() {
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
                assert!(!s.is_empty(), "{pt:?} / {kind:?} template must not be empty");
            }
        }
    }

    #[test]
    fn frontend_react_prompt_has_three_phase_structure() {
        let s = template(ProjectType::FrontendReact, TemplateKind::Prompt);
        assert!(
            s.contains("three-phase") || s.contains("EXPLORE") && s.contains("IMPLEMENT") && s.contains("VERIFY"),
            "frontend-react prompt must describe a three-phase workflow"
        );
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
    fn backend_rust_toml_has_cargo_commands() {
        let s = template(ProjectType::BackendRust, TemplateKind::AgentTomlPartial);
        assert!(s.contains("cargo check"), "backend-rust partial must include cargo check");
        assert!(s.contains("cargo clippy"), "backend-rust partial must include cargo clippy");
        assert!(s.contains("cargo test"), "backend-rust partial must include cargo test");
    }

    #[test]
    fn backend_typescript_toml_has_tsc_and_test() {
        let s = template(ProjectType::BackendTypescript, TemplateKind::AgentTomlPartial);
        assert!(s.contains("tsc"), "backend-typescript partial must include tsc");
        assert!(s.contains("pnpm test"), "backend-typescript partial must include pnpm test");
    }

    #[test]
    fn generic_toml_has_placeholder_comment() {
        let s = template(ProjectType::Generic, TemplateKind::AgentTomlPartial);
        assert!(
            s.contains("Add your quality-check commands"),
            "generic partial must contain placeholder comment"
        );
    }
}
