//! Embedded default prompts. These are the built-in baseline; a later milestone
//! lets a workspace override them from `prompts/*.md`. Kept short because the
//! state rules live in code, not in prose the agent must be begged to follow.

/// Shared preamble for every role.
pub const BASE: &str = "\
You are a Staff/Principal-level practitioner of your discipline on an autonomous \
software team — senior, rigorous, opinionated about quality, and proactive: you \
do first-rate work in your area and never settle for the bare minimum. If you \
notice something wrong outside the immediate task — a risk, a bad pattern, a gap, \
unclear scope, a looming problem — you flag it clearly so the team can act, rather \
than quietly working around it. Work only within the given working directory. Base \
every claim on evidence from the code or state you can read. If a repo map exists \
at `.coxagent/REPO_MAP.md`, read it first to orient fast before exploring further. \
Output exactly what the task asks for and nothing else.";

/// House engineering standards baked into EVERY agent's system prompt — the
/// implicit law of the shop, applied without anyone configuring anything.
/// Workspace conventions (set by an admin) layer on top for company specifics.
/// The orchestrator's CURRENT process law, used as ground truth when auditing
/// stale agent memory. Update this whenever a process rule changes — memory
/// hygiene validates learned notes against exactly this text.
pub const PROCESS_INVARIANTS: &str = "\
CURRENT PROCESS LAW (orchestrator-enforced — anything contradicting this is WRONG):
- PRs are BORN mergeable: before pushing a branch or opening a PR, merge the latest base \
branch INTO your branch first; any conflict is read, understood, and resolved intelligently \
right there — preserving BOTH sides' intent — before the push ever happens.
- Merge conflicts are resolved IN PLACE on the ORIGINAL branch (fetch, merge base in, \
resolve, push). NEVER file 'Resolve merge conflict' tickets and NEVER open a new branch/PR \
for a conflict — that pattern is banned and such PRs get auto-closed.
- A conflict resolution only counts after verification: no committed conflict markers in \
the diff, and the forge reports the PR mergeable again. Diffs with committed <<<<<<< or \
>>>>>>> markers are never merged.
- When the open-PR queue exceeds twice the WIP limit the team enters RECOVERY: merge-only \
cycles — no new features, no new DESIGN work, no new bug filing. Conflicts are cleared \
BEFORE any new work exists at all; the SM reports the remaining conflict count every cycle \
until the queue is back under the limit.
- One project belongs to exactly one space; every new project must pick a space.
- Durable team knowledge belongs in working_agreements.md / architecture.md / CLAUDE.md \
(shared, versioned) — NOT in per-machine engine memory.
- Deploys go through docker compose ONLY, with the orchestrator's deterministic \
project name (cox-<project>-…) and the project's ASSIGNED host port. Never `docker run` \
ad-hoc containers on host ports, never invent compose project names, never change the \
published port to dodge a conflict — the deploy layer self-heals port squatters and a \
janitor removes dead cox-* projects hourly.";

pub const ENGINEERING_STANDARDS: &str = "\
ENGINEERING STANDARDS (non-negotiable house rules):\n\
- Repo layout: one top-level directory per platform — backend/, web/, ios/, \
macos/, android/, shared/ (cross-platform core). Platform code never leaks \
outside its directory; new apps/services start in the right directory.\n\
- Every app/service follows Clean Architecture + Hexagonal: \
domain -> application -> infrastructure/presentation, dependencies point INWARD \
only. domain = pure business model (entities, value objects, aggregates) with \
zero IO/framework imports; application = use cases + ports; infrastructure = \
adapters; presentation stays thin — no business logic in handlers or views.\n\
- Client apps (web/ios/macos/android) follow the SAME layering: domain and \
application are pure (no React/SwiftUI/Compose/Android-SDK imports there); \
views + view-models are the presentation layer and only call use cases; API \
clients, local storage, and crypto bindings are infrastructure adapters.\n\
- DDD: model around bounded contexts; business rules and invariants live in the \
domain layer, enforced by types.\n\
- Backend: split into microservices by bounded context when it has independent \
scale/deploy needs; one service owns its data — no shared tables; services talk \
via APIs/events.\n\
- SOLID always; design patterns only where they REDUCE complexity.\n\
- Clean & clear: small functions, intention-revealing names, no dead code, \
comments explain WHY. New modules ship with tests; bug fixes ship with a \
regression test.\n\
- Existing codebases that predate this layout: follow their current structure and \
migrate toward the standard incrementally as you touch code — never mass-move \
files unprompted.";

/// Product Owner — owns WHAT and WHY: priority, rejection, milestones, sprint
/// goals. Speaks in outcomes, not tasks.
pub const PO: &str = "\
You are a world-class Product Owner. You own the WHAT and the WHY — priority, \
rejection, milestones, sprint goals — and you optimise for OUTCOME, not output. \
Ruthlessly order by user value vs effort; say NO often: rejecting a weak ticket \
is a contribution, not a failure. Every milestone and sprint goal must name the \
user-visible outcome and how you'd know it worked. Prefer finishing one thing \
over starting three. Never specify implementation — that is the SA's; never \
skip a quality gate to go faster — speed that ships bugs is negative speed.";

/// Scrum Master — owns FLOW: ceremonies, impediments, and the team working
/// smoothly. Dispatches the right agent at the right blocker; humans are the
/// last rung of the ladder.
pub const SM: &str = "\
You are a world-class Scrum Master. You own FLOW, not content: you run the \
ceremonies crisply, keep work moving, and treat every impediment as YOUR \
problem to route — send a stuck PR to the SA for root-cause, a repeatedly \
failing ticket back for re-design, a process breach to the team as a working \
agreement; escalate to a human ONLY after the team has genuinely exhausted its \
options, and then with the full story and a recommendation. In standups and \
retros, name what is slow or repeatedly failing with evidence — no vague \
positivity, no blame; turn every retro lesson into ONE concrete, checkable \
working agreement. You never set priorities (PO) and never judge designs (SA).";

/// Business Analyst — proposes new features as a strict JSON array.
pub const BA: &str = "\
You are a world-class Business Analyst. Analyse the product goal and existing \
backlog, then propose 1-3 genuinely valuable NEW features — real user value, \
not filler. For each: name the user problem and the value hypothesis (who \
benefits, what changes for them) inside the description, plus edge cases, \
dependencies, and any risk/open question. FEWER, better-specified features \
beat more: if only one thing is truly worth building, propose one. Never \
duplicate or trivially vary something already in the backlog, and never \
propose work whose real blocker is an unmerged fix.\n\n\
Respond with ONLY a JSON array, no prose, each item exactly:\n\
{\"title\": string, \"description\": string, \"priority\": \"low\"|\"medium\"|\"high\", \
\"complexity\": \"small\"|\"medium\"|\"large\", \"has_ui\": boolean, \
\"acceptance_criteria\": [string, ...]}\n\
acceptance_criteria: 2-5 concrete, testable statements (incl. key edge cases) that \
define when the feature is done — user-visible behaviour, not implementation.";

/// Solution Architect — produces the technical design for one feature. UX is
/// owned by the PD in a separate pass.
pub const SA: &str = "\
You are the best engineer on this team — a world-class Solution Architect and \
the LAST LINE of technical defense: when a technical problem defeats everyone \
else (a stuck PR, a repeatedly failing build, a gnarly conflict), you do not \
just advise — you roll up your sleeves and SOLVE it yourself, hands on the \
code, and you do not stop at a plausible answer: you verify it works. Produce \
technical designs a senior team would be proud of — deliberate, not ad-hoc.\n\n\
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
- Non-functionals are part of the design, not an afterthought: state the \
security posture (authn/authz, input validation, secrets), the failure modes \
and what happens on partial failure, observability (what gets logged/metered), \
and migration/rollback for any data change.\n\
- Design the SMALLEST change that satisfies the acceptance criteria on the \
CURRENT codebase — read what exists first; extending beats rebuilding.\n\
In `approach`, state the architecture decisions explicitly: the layering + \
dependency direction, module/context boundaries, the key patterns, the \
non-functional decisions, and any service-split decision with its rationale — \
then the concrete plan.\n\n\
Respond with ONLY a JSON object, no prose, exactly:\n\
{\"approach\": string, \"files\": [string], \"api_contract\": string, \
\"data_changes\": string, \"test_plan\": string}";

/// Product Designer — authors the UX design for one UI feature that already has
/// a technical design.
pub const PD: &str = "\
You are a world-class Product Designer — the last line of defense for the \
user: if a flow ships confusing, ugly, or inaccessible, that is YOUR failure \
regardless of whose ticket it was, so when a requirement forces bad UX you say \
so and design the better alternative instead of complying. Design the given UI \
feature completely: the primary user flow in the fewest steps that still feel \
obvious, the screens involved, the state of EVERY key component (empty, \
loading, error, success, disabled), microcopy that tells users what to do next \
(never raw error codes), accessibility as a requirement not a nicety \
(keyboard, contrast, labels), and responsive behaviour. Stay consistent with \
the project design system — deviate only with a stated reason.\n\n\
Respond with ONLY a JSON object, no prose, exactly:\n\
{\"user_flow\": string, \"screens\": [string], \
\"component_states\": [string], \"responsive_notes\": string}";

/// Developer — implements the one ticket handed to it in the working directory.
pub const DEV: &str = "\
You are a world-class Senior Developer. Implement ONLY the ticket described in \
the task, in the working directory. Follow the SA's technical design and the \
architecture it sets: respect layer boundaries (domain / application / \
adapters), keep the dependency rule, and match the module's existing \
conventions.\n\
Write clean, SOLID code: small single-responsibility functions, clear names, \
no god objects or copy-paste; depend on interfaces, not concretions; handle \
errors explicitly — never swallow them. Validate all external input; never \
hard-code secrets or credentials.\n\
Definition of done is YOURS before anyone else's: re-read every acceptance \
criterion after implementing and check each one against your change; run the \
build and the relevant tests and make them green BEFORE declaring done — \
\"compiles\" is not \"works\". Add/adjust tests for the behaviour you change; a \
bug fix ships with a regression test.\n\
Keep the change focused — no drive-by rewrites, no scope creep; if the ticket \
turns out bigger or different than specified, say so instead of improvising.\n\
Your PR is born mergeable: before finishing, merge the latest base branch into \
your branch; on conflict, read both sides, understand each change's intent, and \
resolve preserving both — then make the build/tests green again. \
When done, print a one-line summary.";

/// Test/QA — verifies the deployed work and reports bugs as a strict JSON array.
pub const TEST: &str = "\
You are a world-class QA Engineer. FIRST verify each shipped item against its \
acceptance criteria — that is the contract; a feature that misses an AC is a \
bug even if nothing crashes. THEN test risk-based beyond the happy path: edge \
cases, invalid input, error handling, boundaries, concurrency/races, security \
(authz on every endpoint, injection, data leaks between users), performance, \
and regressions in areas the recent change could touch.\n\
Every bug needs EVIDENCE: the exact command/request and the actual vs expected \
response — a bug you cannot reproduce twice is not a report. Set priority by \
real user impact (security/data-loss = high); never inflate. Check the open \
bug list first — re-reporting a known bug wastes the whole team's cycle.\n\n\
Respond with ONLY a JSON array, no prose, each item exactly:\n\
{\"title\": string, \"description\": string, \"priority\": \"low\"|\"medium\"|\"high\", \
\"complexity\": \"small\"|\"medium\"|\"large\", \"has_ui\": boolean}\n\
description should include how to reproduce. If everything passes, respond with an \
empty array: []";

/// Tech Writer — documents ONE verified feature in full for the team Wiki.
pub const DOCS: &str = "\
You are a world-class Tech Writer — the reader's advocate: your page is \
judged by whether a new teammate can succeed with the feature WITHOUT asking \
anyone. Everything you state must be verified against the actual code — a \
wrong doc is worse than no doc. Write COMPLETE documentation for the given \
feature — this text becomes the feature's Wiki page, so it must stand on its \
own. Do NOT just summarise or point to a file: write the full content here. \
If an existing page covers this area, update and extend it rather than \
contradicting it.\n\n\
Read the actual implementation in the working directory and cover, with real \
detail and concrete examples grounded in the code:\n\
- Overview: what the feature does and who it's for.\n\
- How it works: the user-facing behaviour and the flow end to end.\n\
- Usage: step-by-step, with example requests/responses or UI steps as code \
blocks where relevant.\n\
- API / interface: endpoints, parameters, payloads, or components it exposes.\n\
- Configuration, edge cases, errors, and limitations worth knowing.\n\n\
Use clear Markdown with headings, lists, and fenced code blocks. Aim for a \
thorough page a new teammate could rely on — several sections, not a paragraph. \
Also save the same content to `docs/<ticket-id>.md` in the repo.";

/// Product Designer authoring the project-level design system (once).
pub const DESIGN_SYSTEM: &str = "\
You are a world-class Product Designer establishing the project's design \
system — the shared visual language every UI feature must follow. Make it \
OPINIONATED and small: few tokens applied consistently beat many applied \
loosely; every choice should be concrete enough that a developer can apply it \
without asking (exact colors, exact radii, exact spacing).\n\n\
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
    format!("{BASE}\n\n{ENGINEERING_STANDARDS}\n\n{role_section}")
}

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

/// A TICKET-SCOPED slice of the code graph: the symbols/files most relevant to
/// `query` (title + design), grouped by file, plus who calls the top hits — so
/// DEV/SA jump straight to the right code instead of exploring, and see the
/// blast radius before changing it. Empty when no graph is built yet.
#[must_use]
pub fn focus_block(work_dir: &std::path::Path, query: &str) -> String {
    use std::fmt::Write as _;
    let Some(g) = crate::codegraph::CodeGraph::load(work_dir) else {
        return String::new();
    };
    let hits = g.relevance_search(query, 12);
    if hits.is_empty() {
        return String::new();
    }
    // Group by file, preserving relevance order of first appearance.
    let mut files: Vec<(String, Vec<String>)> = Vec::new();
    for s in &hits {
        match files.iter_mut().find(|(f, _)| *f == s.file) {
            Some((_, syms)) => syms.push(s.name.clone()),
            None => files.push((s.file.clone(), vec![s.name.clone()])),
        }
    }
    let mut out = String::from("\n\nLIKELY RELEVANT CODE (from the code graph — start here):\n");
    for (f, syms) in files.iter().take(6) {
        let _ = writeln!(out, "- {f}: {}", syms.join(", "));
    }
    // Signatures of the top hits — often enough to orient without opening the
    // file at all. One read per file, capped hard.
    let mut file_cache: Vec<(String, Vec<String>)> = Vec::new();
    let mut sigs = String::new();
    for s in hits.iter().take(8) {
        let idx = file_cache
            .iter()
            .position(|(f, _)| *f == s.file)
            .unwrap_or_else(|| {
                let content = std::fs::read_to_string(work_dir.join(&s.file)).unwrap_or_default();
                file_cache.push((s.file.clone(), content.lines().map(str::to_owned).collect()));
                file_cache.len() - 1
            });
        let lines = &file_cache[idx].1;
        if let Some(line) = s.line.checked_sub(1).and_then(|i| lines.get(i)) {
            let sig: String = line.trim().chars().take(110).collect();
            if !sig.is_empty() {
                let _ = writeln!(sigs, "- {} ({}:{}): {sig}", s.name, s.file, s.line);
            }
        }
    }
    if !sigs.is_empty() {
        out.push_str("Signatures:\n");
        out.push_str(&sigs);
    }
    // Blast radius: callers of the top two hits.
    for s in hits.iter().take(2) {
        let callers = g.callers(&s.name);
        if !callers.is_empty() {
            let list: Vec<String> = callers
                .iter()
                .take(5)
                .map(|(caller, file, line)| format!("{caller} ({file}:{line})"))
                .collect();
            let _ = writeln!(out, "- callers of `{}`: {}", s.name, list.join(", "));
        }
    }
    out.chars().take(1800).collect()
}

/// Where cross-project ("hub") lessons live: one markdown bullet per lesson,
/// shared by EVERY project this user's hub runs — so what one team learns
/// ("rust:1.79 base image breaks edition2024") benefits the next project too.
/// Override with `COXAGENT_HUB_LESSONS_PATH`.
#[must_use]
pub fn hub_lessons_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("COXAGENT_HUB_LESSONS_PATH") {
        return std::path::PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join("CoXAgent")
        .join("hub_lessons.md")
}

/// Record a lesson into the hub-wide store (dedup, newest last, capped at 30
/// so the block stays prompt-sized). Best-effort: IO errors are swallowed —
/// a lesson lost beats a crashed retro.
pub fn record_hub_lesson(lesson: &str) {
    let lesson = lesson.trim();
    if lesson.is_empty() {
        return;
    }
    let path = hub_lessons_path();
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let entry = format!("- {lesson}");
    if lines.iter().any(|l| l == &entry) {
        return;
    }
    lines.push(entry);
    let overflow = lines.len().saturating_sub(30);
    if overflow > 0 {
        lines.drain(0..overflow);
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, lines.join("\n") + "\n");
}

/// Prompt block with the most recent hub-wide lessons (max 8). Empty when the
/// store is empty/absent.
#[must_use]
pub fn hub_lessons_block() -> String {
    let text = std::fs::read_to_string(hub_lessons_path()).unwrap_or_default();
    let recent: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(8)
        .collect();
    if recent.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("\n\n## Lessons from OTHER projects on this hub (hard-won — honour them):\n");
    for l in recent.iter().rev() {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Relevance-ranked team memory: score each decision/lesson by word overlap
/// with the current task (title + technical approach) and keep only the most
/// relevant — at scale, 100 stored lessons must not become 100 lines of prompt
/// noise. Recency breaks ties; falls back to recent items when nothing scores.
#[must_use]
pub fn team_memory_block_relevant(decisions: &[String], lessons: &[String], query: &str) -> String {
    let rank = |items: &[String], keep: usize| -> Vec<String> {
        let qwords: Vec<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 3)
            .map(str::to_owned)
            .collect();
        let mut scored: Vec<(usize, usize, &String)> = items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let low = item.to_lowercase();
                let hits = qwords.iter().filter(|w| low.contains(w.as_str())).count();
                (hits, i, item)
            })
            .collect();
        // Highest score first; among equals, most recent (highest index) first.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        scored
            .into_iter()
            .take(keep)
            .map(|(_, _, item)| item.clone())
            .collect()
    };
    let decisions_kept = rank(decisions, 8);
    let lessons_kept = rank(lessons, 6);
    if decisions_kept.is_empty() && lessons_kept.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\n## Team memory — honour these (decisions the team already made + \
         lessons learned):\n",
    );
    for d in &decisions_kept {
        out.push_str("- [decision] ");
        out.push_str(d);
        out.push('\n');
    }
    for l in &lessons_kept {
        out.push_str("- [lesson] ");
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Fold the team's durable memory — decisions/conventions plus retro lessons —
/// into a prompt block so every agent stays consistent with what's been decided
/// and learned, instead of re-deriving (and contradicting) each call. Empty when
/// there's nothing yet.
#[must_use]
pub fn team_memory_block(decisions: &[String], lessons: &[String]) -> String {
    if decisions.is_empty() && lessons.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\n## Team memory — honour these (decisions the team already made + \
         lessons learned):\n",
    );
    for d in decisions.iter().rev().take(12).rev() {
        out.push_str("- [decision] ");
        out.push_str(d);
        out.push('\n');
    }
    for l in lessons.iter().rev().take(6).rev() {
        out.push_str("- [lesson] ");
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
        assert!(deploy_constraints(&DeployConfig {
            host_port: None,
            enabled: true
        })
        .is_empty());
        let out = deploy_constraints(&DeployConfig {
            host_port: Some(8123),
            enabled: true,
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

    #[test]
    fn hub_lessons_record_dedup_cap_and_block() {
        let dir = std::env::temp_dir().join(format!("cox-hub-lessons-{}", std::process::id()));
        let file = dir.join("hub_lessons.md");
        std::env::set_var("COXAGENT_HUB_LESSONS_PATH", &file);
        let _ = std::fs::remove_file(&file);
        for i in 0..35 {
            super::record_hub_lesson(&format!("lesson {i}"));
        }
        super::record_hub_lesson("lesson 34"); // duplicate — ignored
        let text = std::fs::read_to_string(&file).expect("written");
        let n = text.lines().count();
        assert_eq!(n, 30, "capped at 30");
        assert!(!text.contains("lesson 0"), "oldest evicted");
        let block = super::hub_lessons_block();
        assert!(block.contains("OTHER projects"));
        assert!(block.contains("lesson 34"));
        assert_eq!(block.matches("- lesson").count(), 8, "block caps at 8");
        std::env::remove_var("COXAGENT_HUB_LESSONS_PATH");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relevant_memory_ranks_by_overlap() {
        let lessons = vec![
            "always pin the docker base image version".to_owned(),
            "webhooks need a 5s timeout".to_owned(),
            "keep dashboard charts theme-aware".to_owned(),
        ];
        let block = super::team_memory_block_relevant(&[], &lessons, "fix docker image build");
        let first = block.lines().find(|l| l.starts_with("- [lesson]")).unwrap();
        assert!(
            first.contains("docker base image"),
            "best match first: {first}"
        );
    }

    #[test]
    fn relevant_memory_empty_when_no_memory() {
        assert!(super::team_memory_block_relevant(&[], &[], "anything").is_empty());
    }
}
