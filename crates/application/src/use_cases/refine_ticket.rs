//! `RefineTicketUseCase` — turns a rough idea into a polished, build-ready
//! ticket by having the product team collaborate: PO (value + priority), SA
//! (technical shape + complexity) and PD (UX) each advise in parallel, then the
//! BA synthesises everything into one clean ticket. Nothing is saved — the user
//! reviews, edits, and decides (Save / Save & Start Flow / Cancel).

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use coxagent_domain::Role;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// One teammate's contribution, surfaced in the UI so the collaboration is visible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamNote {
    pub role: String,
    pub note: String,
}

/// The refined ticket proposal returned for the user to review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinedTicket {
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub complexity: String,
    #[serde(default)]
    pub has_ui: bool,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub team_notes: Vec<TeamNote>,
}

/// Collaborative ticket refinement over the shared engine + store.
pub struct RefineTicketUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    #[allow(dead_code)]
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    /// Compress context and ask agents for terse output (token-saver).
    terse: bool,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RefineTicketUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
            terse: true,
        }
    }

    /// Toggle the token-saver (compress context + terse agent output).
    #[must_use]
    pub fn with_token_saver(mut self, on: bool) -> Self {
        self.terse = on;
        self
    }

    fn terse_tag(&self) -> &'static str {
        if self.terse {
            crate::tokens::TERSE
        } else {
            ""
        }
    }

    /// Refine `idea` into a build-ready ticket. `context` is the project brief.
    ///
    /// # Errors
    /// [`AppError`] when the BA synthesis step fails or returns nothing usable.
    pub async fn execute(&self, idea: &str, context: &str) -> Result<RefinedTicket, AppError> {
        // Trim the project brief to the essential head/tail when the token-saver
        // is on — the advisors don't need the whole document.
        let ctx = if self.terse {
            crate::tokens::compress(context, 6_000)
        } else {
            context.to_owned()
        };
        let context = ctx.as_str();
        let tag = self.terse_tag();
        // Round 1 — the three advisors weigh in concurrently.
        let (po, sa, pd) = tokio::join!(
            self.advise(Role::Po, "PO", PO_GUIDE, idea, context),
            self.advise(Role::Sa, "SA", SA_GUIDE, idea, context),
            self.advise(Role::Pd, "PD", PD_GUIDE, idea, context),
        );

        // Round 2 — the BA synthesises a single clean ticket from the advice.
        let advice = format!(
            "### PO (value & priority)\n{}\n\n### SA (technical shape & complexity)\n{}\n\n\
             ### PD (UX)\n{}",
            blank_if_empty(&po),
            blank_if_empty(&sa),
            blank_if_empty(&pd),
        );
        let task = format!(
            "A stakeholder proposes this idea:\n\n{idea}\n\n\
             The product team has advised:\n\n{advice}\n\n## Project context\n{context}\n\n\
             Synthesise ONE polished, build-ready ticket. Return ONLY a JSON object:\n\
             {{\"title\":string (crisp, imperative),\
             \"description\":string (clear problem + intended outcome, 2-5 sentences),\
             \"priority\":\"high\"|\"medium\"|\"low\",\
             \"complexity\":\"small\"|\"medium\"|\"large\",\
             \"has_ui\":boolean,\
             \"acceptance_criteria\":[3-6 concrete, testable \"done\" conditions],\
             \"team_notes\":[{{\"role\":\"PO\"|\"SA\"|\"PD\",\"note\":one-line takeaway}}]}}\n\
             Ground everything in the idea and the advice; never invent scope the \
             stakeholder didn't ask for.{tag}"
        );
        let request = AgentRequest {
            role: Role::Ba,
            system_prompt: prompts::system_prompt(prompts::BA),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(150),
            escalation_level: 0,
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "BA synthesis failed: {}",
                outcome.stderr.trim()
            ))
            .into());
        }
        let mut ticket = parse_ticket(&outcome.stdout).ok_or_else(|| {
            AppError::from(PortError::Backend(
                "could not parse the refined ticket".to_owned(),
            ))
        })?;
        normalise(&mut ticket);
        // Ensure the advisors' own words survive even if the BA omitted them.
        if ticket.team_notes.is_empty() {
            for (role, text) in [("PO", &po), ("SA", &sa), ("PD", &pd)] {
                if let Some(line) = first_line(text) {
                    ticket.team_notes.push(TeamNote {
                        role: role.to_owned(),
                        note: line,
                    });
                }
            }
        }
        Ok(ticket)
    }

    /// One advisor pass — returns short guidance text, empty on any failure.
    async fn advise(
        &self,
        role: Role,
        _label: &str,
        guide: &str,
        idea: &str,
        context: &str,
    ) -> String {
        let task = format!(
            "{guide}\n\n## The idea\n{idea}\n\n## Project context\n{context}\n\n\
             Reply with 2-4 short bullet points — concrete and specific. No preamble.{}",
            self.terse_tag()
        );
        let request = AgentRequest {
            role,
            system_prompt: prompts::system_prompt(prompts::BASE),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(120),
            escalation_level: 0,
        };
        match self.engine.run(request).await {
            Ok(o) if o.succeeded() => o.stdout.trim().to_owned(),
            _ => String::new(),
        }
    }
}

const PO_GUIDE: &str = "You are the Product Owner. Clarify the user value and who benefits, \
    recommend a priority (high/medium/low) with a one-line reason, and list the business \
    acceptance criteria that define 'done' for the user.";
const SA_GUIDE: &str = "You are the Solution Architect. Note the technical shape (key components/\
    touch-points you can infer), call out risks or unknowns, and recommend a complexity \
    (small/medium/large) with a one-line reason.";
const PD_GUIDE: &str = "You are the Product Designer. Decide whether this needs UI. If it does, \
    list the key UX acceptance criteria (states, edge cases, accessibility). If it does not, say \
    'no UI' and why.";

fn blank_if_empty(s: &str) -> &str {
    if s.trim().is_empty() {
        "(no input)"
    } else {
        s
    }
}

fn first_line(s: &str) -> Option<String> {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .trim_start_matches(['-', '*', '•', ' '])
        .trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// Extract the JSON object from the engine output (tolerant of fences/prose).
fn parse_ticket(raw: &str) -> Option<RefinedTicket> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<RefinedTicket>(&raw[start..=end]).ok()
}

/// Snap free-form values onto the enums the UI expects.
fn normalise(t: &mut RefinedTicket) {
    match t.priority.trim().to_ascii_lowercase().as_str() {
        "high" | "urgent" | "critical" => "high",
        "low" | "minor" => "low",
        _ => "medium",
    }
    .clone_into(&mut t.priority);
    match t.complexity.trim().to_ascii_lowercase().as_str() {
        "small" | "s" | "xs" | "trivial" => "small",
        "large" | "l" | "xl" | "high" => "large",
        _ => "medium",
    }
    .clone_into(&mut t.complexity);
    t.acceptance_criteria.retain(|c| !c.trim().is_empty());
    t.acceptance_criteria.truncate(6);
    t.team_notes.truncate(4);
}
