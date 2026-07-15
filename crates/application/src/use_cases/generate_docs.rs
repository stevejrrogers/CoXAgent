//! `GenerateDocsUseCase` — the DOCS agent writes the project's *living
//! documentation*: a structured set of product + technical pages that both a
//! new human teammate and an AI agent can fully understand. It reads the
//! project brief + shipped work and produces Markdown pages, upserted in place
//! so regeneration refreshes rather than duplicates.

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::state::slugify;
use coxagent_domain::Role;
use serde::Deserialize;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// One page as returned by the engine.
#[derive(Debug, Deserialize)]
struct GenPage {
    #[serde(default)]
    category: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

/// Runs a documentation-generation pass over the shared engine + store.
pub struct GenerateDocsUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> GenerateDocsUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
        }
    }

    /// Generate/refresh the documentation from `context_md` (the project brief)
    /// plus the current tickets. Writes pages into state. Returns the number of
    /// pages written.
    ///
    /// # Errors
    /// [`AppError`] when the engine fails or returns unparseable output.
    pub async fn execute(&self, context_md: &str) -> Result<usize, AppError> {
        let tickets = {
            let state = self.store.load().await?;
            let mut s = String::new();
            for t in state.tickets.iter().take(80) {
                let _ = writeln!(
                    s,
                    "- [{}] {} ({:?}, {:?})",
                    t.id(),
                    t.title(),
                    t.status(),
                    t.ticket_type()
                );
            }
            if s.is_empty() {
                s.push_str("(no tickets yet)");
            }
            s
        };

        let task =
            format!("{PROMPT}\n\n## Project brief\n{context_md}\n\n## Work items\n{tickets}");
        let request = AgentRequest {
            role: Role::Docs,
            system_prompt: SYSTEM.to_owned(),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(240),
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "docs generation failed: {}",
                outcome.stderr.trim()
            ))
            .into());
        }

        let pages = parse_pages(&outcome.stdout)
            .ok_or_else(|| PortError::Backend("docs: could not parse engine output".to_owned()))?;
        if pages.is_empty() {
            return Ok(0);
        }

        let mut state = self.store.load().await?;
        for p in &pages {
            let category = if p.category.eq_ignore_ascii_case("technical") {
                "technical"
            } else {
                "product"
            };
            let title = p.title.trim();
            if title.is_empty() || p.body.trim().is_empty() {
                continue;
            }
            let id = format!("{category}-{}", slugify(title));
            state.upsert_doc(&id, category, title, p.body.trim(), "DOCS");
        }
        let count = state.docs.len();
        self.store.save(&state).await?;
        Ok(count)
    }
}

/// Extract the JSON page array from the engine output (tolerant of code fences
/// or surrounding prose).
fn parse_pages(raw: &str) -> Option<Vec<GenPage>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Vec<GenPage>>(&raw[start..=end]).ok()
}

const SYSTEM: &str = "You are DOCS, the documentation writer on an autonomous software team. \
    You write clear, specific, dual-audience documentation: a new human teammate and an AI \
    coding agent must both be able to read it and act on it. You never invent facts — you \
    document what the brief and the work items actually describe.";

const PROMPT: &str = "Write the project's living documentation as a JSON array of pages. Each \
element is an object: {\"category\": \"product\" | \"technical\", \"title\": string, \"body\": \
string}. `body` is Markdown.

Produce exactly these pages:
PRODUCT
- \"Overview\": what the product is, who it's for, and the problem it solves — in plain language.
- \"Features\": each capability, what it does and why it matters, from the user's point of view.
- \"User Guide\": how a user accomplishes the main tasks, step by step.
TECHNICAL
- \"Architecture\": the system's components and how they interact; include a small text/mermaid \
diagram of the data flow if it helps.
- \"Tech Stack\": languages, frameworks, and key libraries, with a one-line reason for each.
- \"How It Works\": the important end-to-end flows (e.g. request → handler → store), the \
conventions, and where things live.
- \"Development\": how to build, run, test, and safely extend the project.

Rules:
- Be concrete and specific to THIS project — no generic filler.
- Use clear headings, short paragraphs, and bullet lists.
- Define each term on first use; state assumptions explicitly.
- On technical pages, name the real components/files/flows you can infer.
- Optimise for comprehension: the reader should be able to act on every page.
- Output ONLY the JSON array — no prose before or after it.";
