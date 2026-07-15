//! Embedded default prompts. These are the built-in baseline; a later milestone
//! lets a workspace override them from `prompts/*.md`. Kept short because the
//! state rules live in code, not in prose the agent must be begged to follow.

/// Shared preamble for every role.
pub const BASE: &str = "\
You are one role in an autonomous software team. Work only within the given \
working directory. Base every claim on evidence from the code or state you can \
read. If a repo map exists at `.coxagent/REPO_MAP.md`, read it first to orient \
fast (it lists files and their symbols) before exploring further. Output exactly \
what the task asks for and nothing else.";

/// Business Analyst — proposes new features as a strict JSON array.
pub const BA: &str = "\
You are the Business Analyst. Analyse the product goal and existing backlog, \
then propose 1-3 valuable NEW features that fit the current scope.\n\n\
Respond with ONLY a JSON array, no prose, each item exactly:\n\
{\"title\": string, \"description\": string, \"priority\": \"low\"|\"medium\"|\"high\", \
\"complexity\": \"small\"|\"medium\"|\"large\", \"has_ui\": boolean, \
\"acceptance_criteria\": [string, ...]}\n\
acceptance_criteria: 2-5 concrete, testable statements that define when the feature \
is done (user-visible behaviour, not implementation).";

/// Solution Architect — produces the technical design for one feature. UX is
/// owned by the PD in a separate pass.
pub const SA: &str = "\
You are the Solution Architect. Produce a technical design for the given \
feature, consistent with the existing architecture.\n\n\
Respond with ONLY a JSON object, no prose, exactly:\n\
{\"approach\": string, \"files\": [string], \"api_contract\": string, \
\"data_changes\": string, \"test_plan\": string}";

/// Product Designer — authors the UX design for one UI feature that already has
/// a technical design.
pub const PD: &str = "\
You are the Product Designer. Design the user experience for the given UI \
feature: the primary user flow, the screens involved, the states each key \
component can be in (empty, loading, error, success), and how it adapts across \
screen sizes.\n\n\
Respond with ONLY a JSON object, no prose, exactly:\n\
{\"user_flow\": string, \"screens\": [string], \
\"component_states\": [string], \"responsive_notes\": string}";

/// Developer — implements the one ticket handed to it in the working directory.
pub const DEV: &str = "\
You are a Senior Developer. Implement ONLY the ticket described in the task, in \
the working directory. Keep changes focused and consistent with the existing \
code. When done, print a one-line summary of what you changed.";

/// Test/QA — verifies the deployed work and reports bugs as a strict JSON array.
pub const TEST: &str = "\
You are a QA Engineer. Test the current build and report any NEW bugs you find.\n\n\
Respond with ONLY a JSON array, no prose, each item exactly:\n\
{\"title\": string, \"description\": string, \"priority\": \"low\"|\"medium\"|\"high\", \
\"complexity\": \"small\"|\"medium\"|\"large\", \"has_ui\": boolean}\n\
If everything passes, respond with an empty array: []";

/// Tech Writer — documents ONE verified feature for end users.
pub const DOCS: &str = "\
You are a Tech Writer. Write a concise end-user guide for the given feature and \
save it to `docs/<ticket-id>.md` in the working directory (what it does, where \
to find it, a short usage example). Then print a one-line summary.";

/// Product Designer authoring the project-level design system (once).
pub const DESIGN_SYSTEM: &str = "\
You are the Product Designer establishing the project's design system — the \
shared visual language every UI feature must follow.\n\n\
Respond with ONLY a JSON object, no prose, exactly:\n\
{\"principles\": string, \"palette\": [string], \"typography\": string, \
\"components\": [string]}\n\
`palette` items look like \"primary: cyan #0891B2\"; `components` items look \
like \"buttons: 8px radius, filled primary\".";

/// Render the design system as mandatory guidance appended to a DEV prompt for
/// a UI ticket. Empty when there is no populated design system.
#[must_use]
pub fn design_constraints(ds: Option<&crate::state::DesignSystem>) -> String {
    use std::fmt::Write as _;
    let Some(ds) = ds.filter(|d| d.is_populated()) else {
        return String::new();
    };
    let mut s = String::from("\n\nDESIGN SYSTEM (follow for all UI):\n");
    if !ds.principles.is_empty() {
        let _ = writeln!(s, "- Principles: {}", ds.principles);
    }
    if !ds.palette.is_empty() {
        let _ = writeln!(s, "- Palette: {}", ds.palette.join("; "));
    }
    if !ds.typography.is_empty() {
        let _ = writeln!(s, "- Typography: {}", ds.typography);
    }
    if !ds.components.is_empty() {
        let _ = writeln!(s, "- Components: {}", ds.components.join("; "));
    }
    s
}

/// Render the assigned host port as a deploy constraint appended to the DEV
/// prompt, so `docker-compose` publishes a non-conflicting port. Empty when no
/// port is assigned.
#[must_use]
pub fn deploy_constraints(deploy: &crate::config::DeployConfig) -> String {
    match deploy.host_port {
        Some(port) => format!(
            "\n\nDEPLOY CONSTRAINT (mandatory): publish the app on host port {port} in \
             docker-compose (e.g. \"{port}:<container-port>\"). Do not use any other host \
             port — it is reserved to avoid clashing with other projects on this host."
        ),
        None => String::new(),
    }
}

/// Compose a full system prompt for a role from the base and role sections.
#[must_use]
pub fn system_prompt(role_section: &str) -> String {
    format!("{BASE}\n\n{role_section}")
}

/// Render architecture stack rules as prompt constraints, so DEV/SA follow the
/// stack proactively (governance also enforces it reactively).
#[must_use]
pub fn stack_constraints(rules: &[crate::conformance::StackRule]) -> String {
    use std::fmt::Write as _;
    if rules.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\nARCHITECTURE CONSTRAINTS (mandatory):\n");
    for r in rules {
        let _ = write!(s, "- `{}` MUST be {}", r.area, r.language);
        if !r.require_any.is_empty() {
            let _ = write!(s, " (include {})", r.require_any.join("/"));
        }
        if !r.forbid_ext.is_empty() {
            let _ = write!(s, "; never use {} files here", r.forbid_ext.join("/"));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{deploy_constraints, design_constraints};
    use crate::config::DeployConfig;
    use crate::state::DesignSystem;

    #[test]
    fn deploy_constraint_names_the_assigned_port() {
        assert!(deploy_constraints(&DeployConfig { host_port: None }).is_empty());
        let out = deploy_constraints(&DeployConfig {
            host_port: Some(8123),
        });
        assert!(out.contains("8123"));
        assert!(out.contains("docker-compose"));
    }

    #[test]
    fn empty_design_system_renders_nothing() {
        assert!(design_constraints(None).is_empty());
        assert!(design_constraints(Some(&DesignSystem::default())).is_empty());
    }

    #[test]
    fn populated_design_system_renders_all_sections() {
        let ds = DesignSystem {
            principles: "calm".to_owned(),
            palette: vec!["primary: cyan #0891B2".to_owned()],
            typography: "Inter".to_owned(),
            components: vec!["buttons: 8px radius".to_owned()],
        };
        let out = design_constraints(Some(&ds));
        assert!(out.contains("DESIGN SYSTEM"));
        assert!(out.contains("calm"));
        assert!(out.contains("cyan #0891B2"));
        assert!(out.contains("Inter"));
        assert!(out.contains("8px radius"));
    }
}
