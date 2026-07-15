//! `GenerateDocsUseCase` — the DOCS agent writes the project's *living
//! documentation*: a structured set of product + technical pages that both a
//! new human teammate and an AI agent can fully understand. It reads the
//! project brief + shipped work and produces Markdown pages, upserted in place
//! so regeneration refreshes rather than duplicates.

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::state::{slugify, DocPage};
use coxagent_domain::Role;
use serde::Deserialize;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// One page as returned by the engine.
#[derive(Debug, Deserialize)]
struct GenPage {
    /// Folder path, e.g. `"Technical/Architecture"`.
    #[serde(default)]
    folder: String,
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
    /// Compress context and ask agents for terse output (token-saver).
    terse: bool,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> GenerateDocsUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
            terse: true,
        }
    }

    /// Toggle the token-saver (compress context; docs still need full prose so
    /// only the context/tickets embed is trimmed, not the output directive).
    #[must_use]
    pub fn with_token_saver(mut self, on: bool) -> Self {
        self.terse = on;
        self
    }

    /// Generate/refresh the documentation from `context_md` (the project brief)
    /// plus the current tickets. Each section is written by the relevant agent —
    /// DOCS (product), SA (technical + flows), TEST (test cases) — in parallel.
    /// Returns the produced pages (id/folder/category stamped); the caller
    /// persists them via whichever document store is active.
    ///
    /// # Errors
    /// [`AppError`] when every section fails to produce output.
    pub async fn execute(&self, context_md: &str) -> Result<Vec<DocPage>, AppError> {
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

        // With the token-saver on, trim the brief + ticket list embed (the
        // agents write full prose regardless — only the input is compressed).
        let (ctx_owned, tickets) = if self.terse {
            (
                crate::tokens::compress(context_md, 8_000),
                crate::tokens::compress(&tickets, 4_000),
            )
        } else {
            (context_md.to_owned(), tickets)
        };
        let context_md = ctx_owned.as_str();
        let tickets = tickets.as_str();

        // Fan out to the relevant agents concurrently; a section that fails
        // just contributes no pages (the others still land).
        let (product, technical, qa) = tokio::join!(
            self.section(
                Role::Docs,
                "DOCS",
                SYSTEM_DOCS,
                PROMPT_PRODUCT,
                context_md,
                tickets
            ),
            self.section(Role::Sa, "SA", SYSTEM_SA, PROMPT_TECH, context_md, tickets),
            self.section(
                Role::Test,
                "TEST",
                SYSTEM_TEST,
                PROMPT_QA,
                context_md,
                tickets
            ),
        );

        let sections = [("DOCS", product), ("SA", technical), ("TEST", qa)];
        if sections.iter().all(|(_, pages)| pages.is_empty()) {
            return Err(PortError::Backend(
                "docs generation produced nothing (engine unavailable?)".to_owned(),
            )
            .into());
        }

        let now = crate::state::now_rfc3339();
        let mut out: Vec<DocPage> = Vec::new();
        for (agent, pages) in sections {
            for p in pages {
                let folder = p.folder.trim();
                let title = p.title.trim();
                if title.is_empty() || p.body.trim().is_empty() {
                    continue;
                }
                let id = format!("{}-{}", slugify(folder), slugify(title));
                // De-dupe within one pass (last write wins on a repeated id).
                out.retain(|d| d.id != id);
                out.push(DocPage {
                    id,
                    folder: folder.to_owned(),
                    category: cat_from_folder(folder).to_owned(),
                    title: title.to_owned(),
                    body: p.body.trim().to_owned(),
                    updated_at: now.clone(),
                    updated_by: agent.to_owned(),
                });
            }
        }
        Ok(out)
    }

    /// Revise a single page's Markdown per a human instruction (AI edit).
    ///
    /// # Errors
    /// [`AppError`] when the engine fails or returns nothing.
    pub async fn revise(
        &self,
        folder: &str,
        title: &str,
        current: &str,
        instruction: &str,
    ) -> Result<String, AppError> {
        let task = format!(
            "You are editing one documentation page.\n\nFolder: {folder}\nTitle: {title}\n\n\
             ## Current Markdown\n{current}\n\n## Instruction\n{instruction}\n\n\
             Rewrite the FULL page applying the instruction. Keep it accurate to the project \
             (never invent facts), well-structured Markdown, dual-audience (human + AI agent). \
             Output ONLY the new Markdown body — no code fence around the whole thing, no preamble."
        );
        let request = AgentRequest {
            role: Role::Docs,
            system_prompt: SYSTEM_DOCS.to_owned(),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(180),
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(
                PortError::Backend(format!("doc edit failed: {}", outcome.stderr.trim())).into(),
            );
        }
        let body = strip_outer_fence(outcome.stdout.trim());
        if body.is_empty() {
            return Err(PortError::Backend("doc edit produced nothing".to_owned()).into());
        }
        Ok(body)
    }

    /// Run one documentation section with a specific agent role + prompt.
    /// Returns the parsed pages, or an empty vec on any failure.
    async fn section(
        &self,
        role: Role,
        _agent: &str,
        system: &str,
        prompt: &str,
        context_md: &str,
        tickets: &str,
    ) -> Vec<GenPage> {
        let task = format!(
            "{prompt}{RULES}\n\n## Project brief\n{context_md}\n\n## Work items\n{tickets}"
        );
        let request = AgentRequest {
            role,
            system_prompt: system.to_owned(),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(240),
        };
        match self.engine.run(request).await {
            Ok(o) if o.succeeded() => parse_pages(&o.stdout).unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

/// Colour bucket derived from a folder path's top segment.
fn cat_from_folder(folder: &str) -> &'static str {
    match folder
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "technical" => "technical",
        "flows" => "flows",
        "testing" | "qa" | "test" | "tests" => "qa",
        "operations" | "ops" => "ops",
        _ => "product",
    }
}

/// Drop a single ```` ``` ```` fence wrapping the whole body, if present.
fn strip_outer_fence(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        if let Some(end) = rest.rfind("```") {
            // Skip an optional language tag on the first line.
            let inner = &rest[..end];
            let inner = inner.split_once('\n').map_or(inner, |(_, b)| b);
            return inner.trim().to_owned();
        }
    }
    t.to_owned()
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

/// Shared rules appended to every section prompt.
const RULES: &str = "\n\nRules:\n\
- Be concrete and specific to THIS project — no generic filler.\n\
- Use clear headings, short paragraphs, and bullet lists.\n\
- Define each term on first use; state assumptions explicitly.\n\
- Optimise for comprehension: a human OR an AI agent should be able to act on it.\n\
- Never invent facts — if something isn't established, say so.\n\
- Output ONLY the JSON array — no prose before or after it.";

const SYSTEM_DOCS: &str = "You are DOCS, the documentation writer on an autonomous software team. \
    You write clear, specific, dual-audience product documentation a new teammate or an AI agent \
    can act on. You never invent facts.";

const PROMPT_PRODUCT: &str =
    "Write the PRODUCT documentation as a JSON array of pages. Each item: \
{\"folder\":string,\"title\":string,\"body\":markdown}. Put every page in folder \"Product\". \
Produce exactly these titles:\n\
- \"Overview\": what the product is, who it's for, the problem it solves — in plain language.\n\
- \"Features\": each capability, what it does and why it matters, from the user's point of view.\n\
- \"User Guide\": how a user accomplishes the main tasks, step by step.\n\
- \"FAQ\": likely questions with clear answers.";

const SYSTEM_SA: &str = "You are SA (Solution Architect) on an autonomous software team, writing \
    the technical documentation and system flows. Be precise; name real components/files/flows \
    you can infer. You never invent facts.";

const PROMPT_TECH: &str = "Write the TECHNICAL and FLOWS documentation as a JSON array of pages. \
Each item: {\"folder\":string,\"title\":string,\"body\":markdown}. Produce:\n\
folder \"Technical\":\n\
- \"Architecture\": components and how they interact; a small mermaid diagram if it helps.\n\
- \"Tech Stack\": languages, frameworks, key libraries, each with a one-line reason.\n\
- \"How It Works\": important end-to-end paths (e.g. request → handler → store), conventions, \
and where things live.\n\
- \"Data Model\": the main entities/records and their relationships.\n\
- \"Development\": how to build, run, test, and safely extend the project.\n\
folder \"Flows\":\n\
- \"User Flows\": the main user journeys step by step (mermaid flowchart where useful).\n\
- \"System Flows\": key runtime sequences between components (mermaid sequence diagrams help).";

const SYSTEM_TEST: &str = "You are TEST (QA) on an autonomous software team, writing the test \
    documentation. You think in scenarios, expected results, and edge cases. You never invent \
    facts about what exists.";

const PROMPT_QA: &str = "Write the TEST documentation as a JSON array of pages. Each item: \
{\"folder\":string,\"title\":string,\"body\":markdown}. Put every page in folder \"Testing\". \
Produce these titles:\n\
- \"Test Strategy\": what to test and how (levels: unit, integration, e2e), and priorities.\n\
- \"Test Cases\": concrete cases as a checklist/table — for each: scenario, steps, expected \
result. Cover the main features.\n\
- \"Edge Cases & Risks\": boundary conditions, failure modes, and things likely to break.";
