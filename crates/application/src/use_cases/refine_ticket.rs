//! `RefineTicketUseCase` — turns a rough idea into a polished, build-ready
//! ticket by having the product team collaborate: PO (value + priority), SA
//! (technical shape + complexity) and PD (UX) each advise in parallel, then the
//! BA synthesises everything into one clean ticket. Nothing is saved — the user
//! reviews, edits, and decides (Save / Save & Start Flow / Cancel).

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::use_cases::approval_risk::{shape_key, title_is_docish, title_is_testish};
use coxagent_domain::{Role, Status, Ticket};
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

/// A lightweight, human-facing feasibility estimate at FILING time (CXA-F250):
/// what does this idea look like it will cost, and is it worth a design pass?
/// Same 0..100 risk scale and policy shape as the design-time gate
/// ([`crate::use_cases::approval_risk::assess`]) so the preview and the later
/// gate speak one language — but over what is known at filing time only. This
/// is NOT the approval decision; the designed ticket is still gated separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feasibility {
    /// 0 (routine for this team) … 100 (do not file without re-scoping) —
    /// filing risk, in the design-time gate's scale.
    pub score: u8,
    /// `auto` = worth filing as is; `ask` = clarify or re-scope first.
    pub lane: FeasLane,
    /// Same-shape tickets already shipped (Done / Verified / Documented).
    /// Prior art is the strongest evidence a shape is routine here.
    pub prior_art: usize,
    /// What is missing or risky about the idea as filed, in plain language.
    pub gaps: Vec<String>,
}

/// The two filing lanes. Wire form is lowercase (`auto` / `ask`), the same
/// vocabulary the design-time gate announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeasLane {
    Auto,
    Ask,
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
    /// Filing-time feasibility preview (CXA-F250). Produced by the use case
    /// after BA synthesis — never by the BA itself; absent when the refine
    /// fails. Old clients ignore the extra response field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feasibility: Option<Feasibility>,
}

/// Collaborative ticket refinement over the shared engine + store.
pub struct RefineTicketUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
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
            label: None,
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "BA synthesis failed: {}",
                outcome.failure_detail()
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
        // Filing-time feasibility preview (CXA-F250): prior art is a read-only
        // look at what the board already shipped. A store that cannot load
        // simply means no prior art — the refine itself already succeeded, so
        // the preview degrades instead of failing the whole call.
        let shipped = self
            .store
            .load()
            .await
            .map(|s| s.tickets)
            .unwrap_or_default();
        ticket.feasibility = Some(assess_feasibility(
            &ticket,
            count_prior_art(&ticket, &shipped),
        ));
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
            label: None,
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

/// The score line past which an idea should not be filed unclarified — the
/// same line the design-time gate auto-approves under, so what the flier sees
/// previews what the gate will later decide (docs/ADAPTIVE_APPROVAL.md).
const ASK_PAST_SCORE: i32 = 35;

/// Words that say an acceptance criterion speaks about the UX (states, edges,
/// accessibility) rather than pure behaviour — the signal that PD has
/// something to design against when the idea claims UI.
const UX_WORDS: &[&str] = &[
    "ui", "ux", "screen", "click", "tap", "keyboard", "focus", "accessible", "a11y",
    "contrast", "responsive", "layout", "mobile", "hover", "scroll", "viewport", "menu",
    "state", "empty",
];

/// Score the idea AS FILED (CXA-F250). PURE: a function of the refined ticket
/// and the prior-art count — no IO, no model call — so the preview is stable
/// and testable with struct literals. Mirrors the design-time gate's signals
/// that exist before a design: complexity, criteria presence, UI, the title's
/// routine-ness, discounted by prior art.
#[must_use]
fn assess_feasibility(t: &RefinedTicket, prior_art: usize) -> Feasibility {
    let mut score: i32 = 20;
    let mut gaps: Vec<String> = Vec::new();

    match t.complexity.trim().to_ascii_lowercase().as_str() {
        // Large work shapes the system; it pays for a design pass.
        "large" => {
            score += 60;
            gaps.push("large scope — expect a full design pass before build".to_owned());
        }
        "medium" => score += 15,
        "small" => score -= 5,
        _ => {}
    }

    if t.acceptance_criteria.is_empty() {
        score += 25;
        gaps.push("no acceptance criteria yet — write them before filing".to_owned());
    }

    if t.has_ui && !criteria_mention_ux(&t.acceptance_criteria) {
        score += 20;
        gaps.push("UI work with no UX criterion — PD has nothing to design against".to_owned());
    }

    // Test/doc work changes what we KNOW about the system, not what it does —
    // the cheapest class to file, exactly as the design-time gate treats it.
    if title_is_testish(&t.title) || title_is_docish(&t.title) {
        score -= 25;
    }

    // Prior art: each same-shape ticket already shipped is evidence this shape
    // works here, capped so history can never outvote the live signals.
    let credit = i32::try_from(prior_art.min(6)).unwrap_or(0) * 5;
    score -= credit;

    let score = score.clamp(0, 100);
    let lane = if score <= ASK_PAST_SCORE {
        FeasLane::Auto
    } else {
        FeasLane::Ask
    };
    Feasibility {
        score: u8::try_from(score).unwrap_or(100),
        lane,
        prior_art,
        gaps,
    }
}

/// Whether any criterion speaks to UX (states, edges, accessibility).
fn criteria_mention_ux(acs: &[String]) -> bool {
    acs.iter().any(|c| {
        c.to_lowercase()
            .split(|ch: char| !ch.is_alphanumeric())
            .any(|word| UX_WORDS.contains(&word))
    })
}

/// The coarse shape of an idea at filing time, in the same language as
/// [`shape_key`]: the title's test/docs signal, else the feature shape the
/// New-Ticket dialog files ideas as by default, over the BA's complexity.
fn idea_shape(t: &RefinedTicket) -> String {
    let kind = if title_is_testish(&t.title) {
        "test"
    } else if title_is_docish(&t.title) {
        "docs"
    } else {
        "feature"
    };
    format!("{kind}/{}", t.complexity.trim()).to_lowercase()
}

/// How many tickets of the same shape already reached a terminal good state
/// (Done / Verified / Documented). PURE over the shipped history.
#[must_use]
fn count_prior_art(t: &RefinedTicket, shipped: &[Ticket]) -> usize {
    let key = idea_shape(t);
    shipped
        .iter()
        .filter(|s| terminal_good(s.status()) && shape_key(s) == key)
        .count()
}

/// The terminal states that count as shipped prior art. `Fixed` is left out:
/// it is a claim pending regression, not evidence the shape shipped.
fn terminal_good(s: Status) -> bool {
    matches!(s, Status::Done | Status::Documented | Status::Verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Priority, Role, TechnicalDesign, TicketId, TicketType};

    /// A refined idea exactly as the BA synthesis leaves it — struct literals
    /// only, per the house TDD rule.
    fn idea(title: &str, complexity: &str, has_ui: bool, acs: &[&str]) -> RefinedTicket {
        RefinedTicket {
            title: title.to_owned(),
            description: "why it matters".to_owned(),
            priority: "medium".to_owned(),
            complexity: complexity.to_owned(),
            has_ui,
            acceptance_criteria: acs.iter().map(|s| (*s).to_owned()).collect(),
            team_notes: Vec::new(),
            feasibility: None,
        }
    }

    /// A real domain ticket driven along its legal lifecycle to `status`, the
    /// way shipped history actually looks in the store.
    fn shipped(title: &str, kind: TicketType, cx: Complexity, status: Status) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new("CXA-F001").expect("valid id"),
            kind,
            title.to_owned(),
            "body".to_owned(),
            Priority::Medium,
            cx,
            false,
        )
        .expect("valid ticket");
        if matches!(kind, TicketType::Feature | TicketType::Chore) {
            t.set_technical_design(
                Role::Sa,
                TechnicalDesign {
                    approach: "do it".to_owned(),
                    files: vec!["src/w.rs".to_owned()],
                    api_contract: String::new(),
                    test_plan: "cargo test".to_owned(),
                    alternatives: String::new(),
                    data_changes: String::new(),
                },
            )
            .expect("design");
        }
        if kind == TicketType::Bug {
            // A bug with no criteria verifies freely (nothing uncovered).
            let steps: &[Status] = match status {
                Status::Fixed => &[Status::InProgress, Status::Fixed],
                Status::Verified => &[Status::InProgress, Status::Fixed, Status::Verified],
                _ => &[],
            };
            for to in steps {
                t.transition_to(Role::System, *to).expect("legal edge");
            }
        } else {
            let steps: &[Status] = match status {
                Status::Ready => &[Status::Ready],
                Status::InProgress => &[Status::Ready, Status::InProgress],
                Status::Done => &[Status::Ready, Status::InProgress, Status::Done],
                Status::Documented => {
                    &[Status::Ready, Status::InProgress, Status::Done, Status::Documented]
                }
                _ => &[],
            };
            for to in steps {
                t.transition_to(Role::System, *to).expect("legal edge");
            }
        }
        t
    }

    #[test]
    fn large_scope_scores_riskier_than_small() {
        let small = assess_feasibility(&idea("Tighten a log line", "small", false, &["log says x"]), 0);
        let large = assess_feasibility(&idea("Rewrite the scheduler", "large", false, &["it schedules"]), 0);
        assert!(large.score > small.score, "small {small:?} vs large {large:?}");
        assert!(large.gaps.iter().any(|g| g.contains("large scope")));
    }

    #[test]
    fn missing_criteria_surface_as_a_gap() {
        let bare = assess_feasibility(&idea("Add a widget", "medium", false, &[]), 0);
        assert!(bare
            .gaps
            .iter()
            .any(|g| g.contains("no acceptance criteria")), "{bare:?}");
        let with = assess_feasibility(&idea("Add a widget", "medium", false, &["it works"]), 0);
        assert!(!with.gaps.iter().any(|g| g.contains("acceptance criteria")));
    }

    #[test]
    fn ui_without_ux_criteria_surfaces_as_a_gap() {
        let blind = assess_feasibility(
            &idea("Redesign the settings page", "medium", true, &["settings persist"]),
            0,
        );
        assert!(blind.gaps.iter().any(|g| g.contains("UX criterion")), "{blind:?}");
        let designed = assess_feasibility(
            &idea("Redesign the settings page", "medium", true, &["Empty state renders a helper line"]),
            0,
        );
        assert!(!designed.gaps.iter().any(|g| g.contains("UX criterion")));
    }

    #[test]
    fn prior_art_lowers_risk_monotonically() {
        let t = idea("Add a widget", "medium", false, &["it works"]);
        let mut prev = u8::MAX;
        for art in [0usize, 1, 2, 3, 6, 20] {
            let f = assess_feasibility(&t, art);
            assert!(f.score <= prev, "prior art {art} raised the score: {f:?}");
            assert_eq!(f.prior_art, art);
            prev = f.score;
        }
        // The cap holds: history can never outvote the live signals entirely.
        assert_eq!(assess_feasibility(&t, 6).score, assess_feasibility(&t, 500).score);
    }

    #[test]
    fn lane_flips_auto_to_ask_past_the_line() {
        let routine = assess_feasibility(&idea("Add a widget", "small", false, &["it works"]), 2);
        assert_eq!(routine.lane, FeasLane::Auto, "{routine:?}");
        let unclear = assess_feasibility(&idea("Add a widget", "medium", false, &[]), 0);
        assert_eq!(unclear.lane, FeasLane::Ask, "{unclear:?}");
        let big = assess_feasibility(&idea("Rewrite the scheduler", "large", false, &["it schedules"]), 6);
        assert_eq!(big.lane, FeasLane::Ask, "{big:?}");
    }

    #[test]
    fn prior_art_counts_only_terminal_same_shape_tickets() {
        let t = idea("Add a password reset screen", "medium", true, &["reset email sends"]);
        let history = [
            shipped("Add a password reset screen", TicketType::Feature, Complexity::Medium, Status::Done),
            shipped("Add a login screen", TicketType::Feature, Complexity::Medium, Status::Documented),
            shipped("Add a password reset screen", TicketType::Feature, Complexity::Small, Status::Done),
            shipped("Add a password reset screen", TicketType::Feature, Complexity::Medium, Status::Ready),
            shipped("Test coverage: reset screen", TicketType::Feature, Complexity::Medium, Status::Done),
            shipped("Add a password reset screen", TicketType::Bug, Complexity::Medium, Status::Verified),
        ];
        // Done + Documented of the same shape count. A feature idea never
        // matches a bug's Verified (different type in the shape key), a Ready
        // ticket has not shipped, a different size is a different shape, and a
        // test-title ticket is the test class, not the feature class.
        assert_eq!(count_prior_art(&t, &history), 2);
        assert_eq!(count_prior_art(&t, &[]), 0);
    }

    #[test]
    fn feasibility_survives_the_wide_serialization_contract() {
        // The API contract: an old reader must keep parsing a response that
        // carries the new field, and a response without it parses as None.
        let mut t = idea("Add a widget", "small", false, &["it works"]);
        t.feasibility = Some(assess_feasibility(&t, 3));
        let with = serde_json::to_value(&t).expect("serialize");
        assert_eq!(with["feasibility"]["lane"], "auto");
        assert_eq!(with["feasibility"]["prior_art"], 3);
        assert!(serde_json::from_value::<RefinedTicket>(with).is_ok());
        let without = serde_json::json!({
            "title": "x", "description": "y"
        });
        let parsed: RefinedTicket = serde_json::from_value(without).expect("old payload");
        assert!(parsed.feasibility.is_none());
    }

    // --- execute() wiring: the same MemStore/CannedEngine doubles the sibling
    // --- use cases test with — in-process, no server, no harness.

    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
    use crate::state::ProjectState;
    use crate::PortError;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }

    #[async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, _state: &ProjectState) -> Result<(), PortError> {
            Err(PortError::Backend("read-only double".to_owned()))
        }
    }

    /// A store whose every load fails — the degradation path for the preview.
    struct DownStore;

    #[async_trait]
    impl StateStorePort for DownStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Err(PortError::Backend("store down".to_owned()))
        }
        async fn save(&self, _state: &ProjectState) -> Result<(), PortError> {
            Err(PortError::Backend("store down".to_owned()))
        }
    }

    struct CannedEngine {
        stdout: String,
    }

    #[async_trait]
    impl AgentEnginePort for CannedEngine {
        fn id(&self) -> &'static str {
            "canned"
        }
        async fn run(&self, _req: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: self.stdout.clone(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
                engine: String::new(),
                model: String::new(),
                attempts: Vec::new(),
            })
        }
    }

    fn canned_ba_json() -> String {
        r#"{"title":"Add a password reset screen","description":"users are locked out",
            "priority":"high","complexity":"medium","has_ui":true,
            "acceptance_criteria":["Reset email sends"],
            "team_notes":[{"role":"SA","note":"one endpoint"}]}"#
            .to_owned()
    }

    #[tokio::test]
    async fn execute_attaches_a_feasibility_verdict_from_store_prior_art() {
        let mut state = ProjectState::default();
        state
            .tickets
            .push(shipped("Add a login screen", TicketType::Feature, Complexity::Medium, Status::Done));
        let store = Arc::new(MemStore {
            state: Mutex::new(state),
        });
        let engine = Arc::new(CannedEngine {
            stdout: canned_ba_json(),
        });
        let uc = RefineTicketUseCase::new(store, engine, PathBuf::from("/tmp"));
        let t = uc.execute("users locked out", "").await.expect("refine");
        let f = t.feasibility.expect("preview attached");
        assert_eq!(f.prior_art, 1, "the Done feature/medium ticket is prior art");
        assert_eq!(f.lane, FeasLane::Ask, "medium UI idea with one criterion still asks");
        assert!(f.score > 0 && f.score <= 100);
    }

    #[tokio::test]
    async fn execute_degrades_to_no_prior_art_when_the_store_is_down() {
        let engine = Arc::new(CannedEngine {
            stdout: canned_ba_json(),
        });
        let uc = RefineTicketUseCase::new(Arc::new(DownStore), engine, PathBuf::from("/tmp"));
        let t = uc.execute("users locked out", "").await.expect("refine");
        let f = t.feasibility.expect("preview still attached");
        assert_eq!(f.prior_art, 0, "a dead store must not fail the refine");
    }
}
