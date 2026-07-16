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
You are a Staff Solution Architect. Produce a technical design that a senior \
team would be proud of — deliberate, not ad-hoc.\n\n\
Design principles (apply with judgement, sized to the feature — don't \
over-engineer a small change):\n\
- Clean/Hexagonal architecture: a pure domain core, application/use-case layer, \
and adapters (HTTP, DB, UI) at the edges. Dependencies point INWARD; the domain \
depends on nothing external.\n\
- DDD when the domain is non-trivial: clear bounded contexts, aggregates that \
guard invariants, value objects, and ubiquitous language reflected in names.\n\
- SOLID and separation of concerns, per module (FE / BE / shared). Small, \
single-responsibility units; program to interfaces (ports), not implementations.\n\
- Design patterns used deliberately where they fit (repository, strategy, \
factory, adapter, CQRS…) — never cargo-culted.\n\
- Service boundaries: default to a well-structured MODULAR MONOLITH. Propose \
microservices ONLY with explicit justification (independent scaling, separate \
deploy/ownership, distinct data stores) — and say why.\n\
In `approach`, state the architecture decisions explicitly: the layering + \
dependency direction, module/context boundaries, the key patterns, and any \
service-split decision with its rationale — then the concrete plan.\n\n\
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
the working directory. Follow the SA's technical design and the architecture it \
sets: respect layer boundaries (domain / application / adapters), keep the \
dependency rule (edges depend on the core, never the reverse), and match the \
module's existing conventions.\n\
Write clean, SOLID code: small single-responsibility functions, clear names, no \
god objects or copy-paste; depend on interfaces, not concretions; handle errors \
explicitly. Add/adjust tests for the behaviour you change. Keep the change \
focused — no drive-by rewrites. When done, print a one-line summary.";

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

/// Language directive appended to Scrum-ceremony prompts so the whole standup /
/// planning / grooming / retro conversation reads in Vietnamese. Structural
/// keywords the UI keys off (e.g. `BLOCKER:`) are kept verbatim on purpose.
pub const VI_REPLY: &str =
    " Viết toàn bộ phản hồi bằng tiếng Việt tự nhiên (giữ nguyên các nhãn kỹ thuật \
     như \"BLOCKER:\" và mã ticket).";

/// A compact repo-map context block for code-touching agents: the file/symbol
/// layout so they locate code without exploring blind (fewer tool calls / tokens).
/// Empty when the token-saver is off or no map has been built yet.
#[must_use]
pub fn repo_map_block(work_dir: &std::path::Path, enabled: bool) -> String {
    if !enabled {
        return String::new();
    }
    let path = work_dir.join(".coxagent").join("REPO_MAP.md");
    let Ok(map) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let compact: String = map.chars().take(3000).collect();
    format!(
        "\n\n## Repo map — files & their symbols (use this to locate code fast, \
         don't re-scan the whole tree)\n{compact}\n"
    )
}

/// Fold the team's retro lessons into a prompt block, so agents actually apply
/// what past sprints learned. Empty when there are none.
#[must_use]
pub fn lessons_block(lessons: &[String]) -> String {
    if lessons.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nLessons the team learned in past retros — apply them:\n");
    for l in lessons.iter().rev().take(6).rev() {
        out.push_str("- ");
        out.push_str(l);
        out.push('\n');
    }
    out
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
    use super::{deploy_constraints, design_constraints, repo_map_block};
    use crate::config::DeployConfig;
    use crate::state::DesignSystem;

    #[test]
    fn repo_map_block_gated_and_present() {
        let dir = std::env::temp_dir().join(format!("rmb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".coxagent")).expect("mk");
        std::fs::write(
            dir.join(".coxagent/REPO_MAP.md"),
            "# Repo map\n## src/x.rs (rust)\n  fn go",
        )
        .expect("w");
        assert!(repo_map_block(&dir, false).is_empty(), "off = empty");
        let on = repo_map_block(&dir, true);
        assert!(on.contains("src/x.rs") && on.contains("Repo map"));
        // Missing map = empty even when enabled.
        assert!(repo_map_block(std::path::Path::new("/no/such/dir"), true).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
