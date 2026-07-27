//! `RunDocsUseCase` — the DOCS agent. Documents one `Done` feature (writing a
//! user guide into the codebase) and moves it to `Documented`. Runs after TEST
//! so only completed work is documented.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::selection::documentable_candidates;
use coxagent_domain::{Role, Status, TicketId};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Runs one documentation pass.
pub struct RunDocsUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    worker: String,
    phase: Option<crate::use_cases::runner::PhaseReporter>,
    context: Option<String>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunDocsUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            worker: String::new(),
            phase: None,
            context: None,
        }
    }

    #[must_use]
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// Set this runner's identity (`account@host`) so the DOCS stage is claimed
    /// per-ticket for parallel-safe documentation across concurrent runners.
    #[must_use]
    pub fn with_worker(mut self, worker: impl Into<String>) -> Self {
        self.worker = worker.into();
        self
    }

    /// Attach the live "working now" reporter; fired only after the stage is won.
    #[must_use]
    pub fn with_phase(mut self, phase: Option<crate::use_cases::runner::PhaseReporter>) -> Self {
        self.phase = phase;
        self
    }

    /// Document the next `Done` feature. Returns its id, or `None` when there's
    /// nothing to document.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or an unexpected transition error.
    #[allow(clippy::too_many_lines)] // one linear pass: claim, write, gate, publish
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        let state = self.store.load().await?;
        let worker = if self.worker.is_empty() {
            "local".to_owned()
        } else {
            self.worker.clone()
        };
        let now = crate::state::now_rfc3339();
        let mut chosen = None;
        for cand in documentable_candidates(&state) {
            if self.store.claim_stage(&cand, "docs", &worker, &now).await? {
                chosen = Some(cand);
                break;
            }
        }
        let Some(id) = chosen else {
            return Ok(None);
        };
        if let Some(p) = &self.phase {
            p(Some(("DOCS".to_owned(), id.to_string())));
        }
        let ticket_type = state.ticket(&id).map_or(
            coxagent_domain::TicketType::Feature,
            coxagent_domain::Ticket::ticket_type,
        );
        let title = state
            .ticket(&id)
            .map_or("", coxagent_domain::Ticket::title)
            .to_owned();

        // The standard space for this ticket type, plus the sub-folders that
        // already exist under it — so the agent reuses one rather than inventing
        // a redundant sibling. This keeps the tree tidy instead of "lung tung".
        let ticket_desc = state
            .ticket(&id)
            .map_or("", coxagent_domain::Ticket::description)
            .to_owned();
        let space = crate::state::doc_space_for(ticket_type, &title, &ticket_desc);
        let prefix = format!("{space}/");
        let existing_subs: Vec<String> = state
            .doc_folders
            .iter()
            .filter_map(|f| f.strip_prefix(&prefix))
            .filter(|s| !s.contains('/'))
            .map(ToOwned::to_owned)
            .collect();

        let _choice = self.config.engine.resolve(Role::Docs);
        let context_block = self
            .context
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(|c| {
                format!("\n\n## Project context (goal, stack — doc within this framing):\n{c}\n")
            })
            .unwrap_or_default();
        let repo_map = prompts::repo_map_block(&self.work_dir, self.config.workflow.token_saver);
        let focus = prompts::focus_block(&self.work_dir, &title);
        // Revising beats rewriting: when a page already covers this area the
        // agent must see it, or "update" silently becomes "replace" and the
        // page loses everything the last ticket documented.
        let current_page = existing_page_for(&state, space, &title, &ticket_desc)
            .map(|p| (p.id.clone(), p.title.clone(), p.body.clone()));
        let outcome = self
            .engine
            .run(AgentRequest {
                role: Role::Docs,
                system_prompt: prompts::system_prompt(prompts::DOCS),
                task_prompt: format!(
                    "{}{context_block}{focus}{repo_map}",
                    build_docs_prompt(
                        &id,
                        &title,
                        space,
                        &existing_subs,
                        current_page
                            .as_ref()
                            .map(|(_, t, b)| (t.as_str(), b.as_str())),
                    )
                ),
                work_dir: self.work_dir.clone(),
                timeout: Duration::from_secs(900),
                escalation_level: 0,
            })
            .await?;
        if !outcome.succeeded() {
            self.store.release_stage(&id, "docs", &worker).await.ok();
            return Err(PortError::Backend(format!(
                "DOCS engine failed on {id}: {}",
                outcome.failure_detail()
            ))
            .into());
        }

        // Structure gate: a page that skips the skeleton is unreadable to the
        // next agent and unsearchable for people. One bounded repair pass, the
        // same deal the code gates give a developer.
        let mut raw = outcome.stdout.clone();
        if let Some(missing) = docs_gate_failures(&raw) {
            let fixup = format!(
                "Your page for {id} is missing required parts: {missing}.\n\nOutput the COMPLETE \
                 page again with the full skeleton — same `FOLDER:` first line, every required \
                 heading verbatim, a `**Keywords:**` line, and a `## Code map` listing the real \
                 files. Do not drop anything you already wrote."
            );
            let repair = self
                .engine
                .run(AgentRequest {
                    role: Role::Docs,
                    system_prompt: prompts::system_prompt(prompts::DOCS),
                    task_prompt: fixup,
                    work_dir: self.work_dir.clone(),
                    timeout: Duration::from_secs(900),
                    escalation_level: 0,
                })
                .await;
            if let Ok(o) = repair {
                if o.succeeded() && docs_gate_failures(&o.stdout).is_none() {
                    raw = o.stdout;
                }
            }
        }
        if let Some(missing) = docs_gate_failures(&raw) {
            // Publishing a page that fails the gate teaches the wiki's readers
            // that the skeleton is optional. Leave the ticket for the next
            // cycle instead, and say why.
            self.store.release_stage(&id, "docs", &worker).await.ok();
            tracing::warn!("DOCS page for {id} rejected by the structure gate: {missing}");
            return Ok(None);
        }

        // Split the leading `FOLDER: <topic>` hint off the body, then map it to a
        // clean sub-folder — reusing an existing one when it matches, dropping it
        // when empty/junk so a page never creates a stray folder.
        let (sub, doc_body) = parse_folder_hint(&raw);
        let sub = sub.and_then(|s| sanitize_subfolder(&s, &existing_subs));
        let folder = match &sub {
            Some(s) => format!("{space}/{s}"),
            None => space.to_owned(),
        };
        let category = crate::state::doc_category_of(space);

        // Surface the documentation in the Wiki: one page per documented feature,
        // so the knowledge base actually fills up as the team ships (not just
        // markdown buried in the codebase).
        let body = if doc_body.trim().len() > 40 {
            doc_body.trim().to_owned()
        } else {
            format!("Documentation for **{title}** ({id}). See the codebase docs for details.")
        };
        let has_sub = sub.is_some();
        // One page per AREA, not per ticket. Keying the page on the ticket id
        // meant a feature's page could never be revised — the next ticket in
        // the same area wrote a second page beside it, and the wiki grew a
        // parallel history instead of a current answer.
        let (page_id, page_title) = current_page.as_ref().map_or_else(
            || (format!("feat-{id}"), title.clone()),
            |(pid, ptitle, _)| (pid.clone(), ptitle.clone()),
        );
        // Atomic read-modify-write with retry (parallel-safe).
        crate::ports::outbound::mutate_state(self.store.as_ref(), |state| {
            state.ensure_standard_folders();
            let ticket = state
                .ticket_mut(&id)
                .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished")))?;
            ticket
                .transition_to(Role::Docs, Status::Documented)
                .map_err(|e| PortError::Corrupt(e.to_string()))?;
            if has_sub {
                state.add_doc_folder(&folder);
            }
            state.upsert_doc(&page_id, &folder, category, &page_title, &body, "DOCS");
            Ok(())
        })
        .await?;
        if let Some(p) = &self.phase {
            p(None);
        }
        Ok(Some(id))
    }
}

/// What the page is missing, or `None` when it satisfies the skeleton every
/// page is expected to share. Kept mechanical on purpose: an LLM judging its
/// own prose is not a gate.
fn docs_gate_failures(raw: &str) -> Option<String> {
    let body = parse_folder_hint(raw).1;
    let text = body.trim();
    let mut missing: Vec<&str> = Vec::new();
    for heading in [
        "## Overview",
        "## How it works",
        "## Usage",
        "## Interface",
        "## Configuration",
        "## Edge cases and limits",
        "## Code map",
    ] {
        // Case-insensitive: the heading text is what matters, not its casing.
        if !text.to_lowercase().contains(&heading.to_lowercase()) {
            missing.push(heading);
        }
    }
    let mut problems: Vec<String> = Vec::new();
    if !missing.is_empty() {
        problems.push(format!("missing headings: {}", missing.join(", ")));
    }
    if !text.to_lowercase().contains("**keywords:**") {
        problems.push("no `**Keywords:**` line (nothing to search on)".to_owned());
    }
    // A Code map with no path is decoration; require something that looks like
    // one so an agent can actually jump to the code.
    let has_path = text
        .lines()
        .any(|l| l.trim_start().starts_with('-') && l.contains('/') && l.contains('.'));
    if !has_path {
        problems.push("`## Code map` lists no real file paths".to_owned());
    }
    if text.chars().count() < 700 {
        problems.push(format!("too thin at {} characters", text.chars().count()));
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// The page that already documents this ticket's area, if the team wrote one.
/// Matched on distinctive title/body terms within the same space, with a floor
/// so a passing word never hijacks an unrelated page.
fn existing_page_for<'a>(
    state: &'a crate::state::ProjectState,
    space: &str,
    title: &str,
    description: &str,
) -> Option<&'a crate::state::DocPage> {
    let terms = {
        let mut t = crate::codegraph::tokenize(&format!("{title} {description}"));
        t.retain(|w| w.len() > 3);
        t.sort();
        t.dedup();
        t
    };
    if terms.is_empty() {
        return None;
    }
    let score = |text: &str| -> usize {
        let have = crate::codegraph::tokenize(text);
        terms.iter().filter(|q| have.contains(q)).count()
    };
    let mut best: Option<(usize, &crate::state::DocPage)> = None;
    for p in &state.docs {
        if !p.folder.starts_with(space) {
            continue;
        }
        // The title carries the area; the body is corroboration only.
        let s = score(&p.title) * 3 + score(&p.body).min(6);
        if s >= 6 && best.is_none_or(|(b, _)| s > b) {
            best = Some((s, p));
        }
    }
    best.map(|(_, p)| p)
}

/// Build the DOCS task prompt, asking the agent to first pick the best
/// sub-folder for the page under its space — reusing an existing one when it
/// fits, only proposing a new concise topic when none do, so the Wiki stays
/// organised rather than sprouting redundant folders.
fn build_docs_prompt(
    id: &TicketId,
    title: &str,
    space: &str,
    existing: &[String],
    current: Option<(&str, &str)>,
) -> String {
    let subs = if existing.is_empty() {
        "(none yet)".to_owned()
    } else {
        existing.join(", ")
    };
    if let Some((page_title, body)) = current {
        // Cap the page we hand back: a long page would otherwise dominate the
        // prompt, and the agent can read the rest in the wiki.
        let excerpt: String = body.chars().take(6000).collect();
        return format!(
            "Ticket {id} ({title}) shipped in an area this Wiki ALREADY documents.\n\n\
             REVISE the existing page below so it describes the CURRENT behaviour: keep what is \
             still true, correct what this ticket changed, add what is new, and remove what is \
             now wrong. Do not start a new document and do not contradict the parts you keep — \
             the result must read as one coherent page, not an append log.\n\n\
             On the FIRST line output exactly `FOLDER: <topic>` (reuse one of: {subs}, or `-` for \
             the space root). Then output the COMPLETE revised page in Markdown — it replaces the \
             old one, so anything you omit is lost.\n\n\
             === EXISTING PAGE: {page_title} ===\n{excerpt}"
        );
    }
    format!(
        "Document ticket {id}: {title}\n\n\
         This page lives in the \"{space}\" Wiki space. Existing sub-folders there: {subs}.\n\
         On the FIRST line, output exactly `FOLDER: <topic>` naming the best home for this page: \
         reuse one of the existing sub-folders when it fits; only propose a NEW short topic \
         (2-3 words, Title Case, e.g. \"Authentication\", \"Messaging\") when none fit; or write \
         `FOLDER: -` to leave it at the space root. Do NOT invent redundant or one-off folders.\n\
         Then, from the next line on, write the page in Markdown, following the required \
         skeleton exactly: `# <Area>`, a `**Keywords:**` line, then `## Overview`, \
         `## How it works`, `## Usage`, `## Interface`, `## Configuration`, \
         `## Edge cases and limits`, `## Code map` (real file paths — this is how an agent \
         finds the code), `## Related`."
    )
}

/// Split a leading `FOLDER: <topic>` line off the agent output. Returns the
/// raw topic (if present and not the `-` sentinel) and the remaining body.
fn parse_folder_hint(stdout: &str) -> (Option<String>, String) {
    let trimmed = stdout.trim_start();
    let Some(rest) = trimmed
        .strip_prefix("FOLDER:")
        .or_else(|| trimmed.strip_prefix("Folder:"))
    else {
        return (None, stdout.to_owned());
    };
    let (line, body) = rest.split_once('\n').unwrap_or((rest, ""));
    let topic = line.trim();
    let hint = if topic.is_empty() || topic == "-" {
        None
    } else {
        Some(topic.to_owned())
    };
    (hint, body.to_owned())
}

/// Clean an agent-proposed sub-folder into a safe, tidy name, or `None` to file
/// at the space root. Keeps letters/digits/spaces/hyphens only, collapses
/// whitespace, caps the length, and — case-insensitively — reuses an existing
/// sub-folder name so "auth" and "Auth" never split into two.
fn sanitize_subfolder(raw: &str, existing: &[String]) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return None;
    }
    let name: String = if joined.len() > 40 {
        joined
            .chars()
            .take(40)
            .collect::<String>()
            .trim_end()
            .to_owned()
    } else {
        joined
    };
    // Reuse an existing folder that matches case-insensitively, so "auth" and
    // "Auth" never split the tree into two near-duplicate folders.
    Some(
        existing
            .iter()
            .find(|e| e.eq_ignore_ascii_case(&name))
            .cloned()
            .unwrap_or(name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
    use crate::state::ProjectState;
    use coxagent_domain::{Complexity, Priority, TechnicalDesign, Ticket, TicketType};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }
    #[async_trait::async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
            s.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    struct OkEngine;
    #[async_trait::async_trait]
    impl AgentEnginePort for OkEngine {
        fn id(&self) -> &'static str {
            "ok"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                // A page that satisfies the structure gate — the contract every
                // real DOCS run has to meet.
                stdout: SKELETON_PAGE.to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
            })
        }
    }

    fn done_feature(id: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t");
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("d");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.transition_to(Role::DevFeature, Status::InProgress)
            .expect("claim");
        t.transition_to(Role::DevFeature, Status::Done)
            .expect("done");
        t
    }

    #[test]
    fn parses_folder_hint_and_body() {
        let (sub, body) = parse_folder_hint("FOLDER: Messaging\n# Guide\nbody");
        assert_eq!(sub.as_deref(), Some("Messaging"));
        assert_eq!(body.trim(), "# Guide\nbody");
        // Sentinel and missing header both yield no sub-folder.
        assert_eq!(parse_folder_hint("FOLDER: -\nx").0, None);
        assert_eq!(parse_folder_hint("no header here").0, None);
    }

    #[test]
    fn sanitize_reuses_and_cleans() {
        let existing = vec!["Authentication".to_owned()];
        // Case-insensitive reuse: "auth"→ existing "Authentication"? No — only an
        // exact case-insensitive match reuses; "authentication" does.
        assert_eq!(
            sanitize_subfolder("authentication", &existing).as_deref(),
            Some("Authentication")
        );
        // Junk characters are stripped; a clean topic survives.
        assert_eq!(
            sanitize_subfolder("Push/Notifications!!", &[]).as_deref(),
            Some("Push Notifications")
        );
        // Empty after cleaning → no folder.
        assert_eq!(sanitize_subfolder("///", &[]), None);
    }

    /// A minimal page that passes `docs_gate_failures`, for engines under test.
    const SKELETON_PAGE: &str = "FOLDER: -\n# Messaging\n**Keywords:** chat, message, send\n\
         ## Overview\nIt sends messages between teammates, reliably and in order, so a \
         conversation reads the same for everyone who opens it later on. Delivery is \
         at-least-once and the channel log is the record of truth, so a client that \
         reconnects replays what it missed rather than guessing. Direct messages and \
         channel posts share this path; only the addressing differs.\n\
         ## How it works\nsend_message() appends to the channel log.\n\
         ## Usage\nPost to the channel.\n## Interface\nPOST /api/chat\n\
         ## Configuration\nretention_days, default 90\n\
         ## Edge cases and limits\nA dropped socket retries once.\n\
         ## Code map\n- crates/application/src/use_cases/run_chat_reply.rs — the reply loop\n\
         ## Related\nCOX-F001\n";

    #[tokio::test]
    async fn documents_done_feature() {
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                tickets: vec![done_feature("F001")],
                ..ProjectState::default()
            }),
        });
        let uc = RunDocsUseCase::new(
            Arc::clone(&store),
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
        );
        let id = uc.execute().await.expect("run");
        assert_eq!(id.expect("some").as_str(), "F001");
        assert_eq!(
            store.load().await.expect("load").tickets[0].status(),
            Status::Documented
        );
    }

    #[tokio::test]
    async fn nothing_to_document_is_none() {
        let store = Arc::new(MemStore::default());
        let uc = RunDocsUseCase::new(
            store,
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
        );
        assert!(uc.execute().await.expect("run").is_none());
    }
}

#[cfg(test)]
mod docs_gate_tests {
    use super::docs_gate_failures;

    fn full_page() -> String {
        format!(
            "FOLDER: Deploys\n# Deploy health gate\n**Keywords:** deploy, health, gate, port\n\
             ## Overview\n{filler}\n## How it works\nverify_deploy_health() polls the port.\n\
             ## Usage\nRun the deploy.\n## Interface\nGET /api/health\n## Configuration\n\
             health_check_timeout_secs, default 60\n## Edge cases and limits\n\
             An unreachable endpoint fails the gate.\n## Code map\n\
             - crates/application/src/ports/outbound/deploy.rs — the gate itself\n\
             ## Related\nCOX-B004\n",
            filler = "It gates every deploy behind a real health probe. ".repeat(12)
        )
    }

    #[test]
    fn a_complete_page_passes() {
        assert_eq!(docs_gate_failures(&full_page()), None);
    }

    #[test]
    fn a_summary_paragraph_is_rejected_with_reasons() {
        let thin = "FOLDER: -\nDocumentation for the health check. See the code for details.\n";
        let why = docs_gate_failures(thin).expect("must be rejected");
        assert!(why.contains("missing headings"), "{why}");
        assert!(why.contains("Keywords"), "{why}");
        assert!(why.contains("too thin"), "{why}");
    }

    #[test]
    fn a_code_map_without_paths_does_not_count() {
        // The section exists but points nowhere — useless to the agent that
        // has to find the code.
        let page = full_page().replace(
            "- crates/application/src/ports/outbound/deploy.rs — the gate itself",
            "- the deploy port module",
        );
        let why = docs_gate_failures(&page).expect("must be rejected");
        assert!(why.contains("no real file paths"), "{why}");
    }
}
