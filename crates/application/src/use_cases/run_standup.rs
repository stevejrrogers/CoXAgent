//! `RunStandupUseCase` — a real daily standup, run by the SM with live agent
//! turns. The SM opens with the sprint goal and status, each participating role
//! gives a grounded update (done / next / blockers), and the SM closes by
//! highlighting blockers and the focus for the day. Every turn is posted to the
//! Scrum feed so the ceremony reads like a human team, not a status dump.

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use coxagent_domain::Role;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Pick the teammate best placed to resolve a blocker from its wording — so the
/// right role owns the follow-up instead of a generic reply.
fn responder_for(blocker_lower: &str) -> &'static str {
    let has = |kw: &[&str]| kw.iter().any(|k| blocker_lower.contains(k));
    if has(&["pr", "merge", "conflict", "rebase", "review"]) {
        "DEV-FEATURE"
    } else if has(&["deploy", "port", "docker", "build", "ci"]) {
        "DEV-BUG"
    } else if has(&["design", "ux", "layout", "screen"]) {
        "PD"
    } else if has(&["spec", "requirement", "acceptance", "scope", "unclear"]) {
        "BA"
    } else if has(&["test", "qa", "flaky", "coverage"]) {
        "TEST"
    } else {
        "SA"
    }
}

/// The role personas pulled into a standup, in order.
const PARTICIPANTS: &[(&str, &str)] = &[
    ("BA", "Business Analyst — backlog & requirements"),
    ("SA", "Solution Architect — design & technical risk"),
    ("PD", "Product Designer — UX"),
    ("DEV-FEATURE", "Feature Developer"),
    ("DEV-BUG", "Bug-fix Developer"),
    ("TEST", "QA Engineer"),
    ("DOCS", "Tech Writer"),
];

/// Runs a facilitated standup over the shared engine + store.
pub struct RunStandupUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    lang: crate::config::Language,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunStandupUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
            lang: crate::config::Language::En,
        }
    }

    /// Set the language the ceremony speaks (English or Vietnamese).
    #[must_use]
    pub fn with_language(mut self, lang: crate::config::Language) -> Self {
        self.lang = lang;
        self
    }

    /// Run the standup. Returns the number of blockers agents raised.
    ///
    /// # Errors
    /// [`AppError`] when the engine fails on a turn.
    pub async fn execute(&self) -> Result<usize, AppError> {
        let context = self.status_context().await;

        // SM opens the standup for real: a short spoken intro on where the sprint
        // stands and a prompt for the team to report in — not just a status line.
        let opener = self.opener(&context).await?;
        self.post("SM", &format!("🗣️ Standup — {}", opener.trim()))
            .await;

        // Only pull in roles that actually did something (grounded, not noise).
        let active = self.active_roles().await;
        let mut updates: Vec<(String, String)> = Vec::new();
        let mut raised: Vec<(String, String)> = Vec::new();
        for (role, persona) in PARTICIPANTS.iter().filter(|(r, _)| active.contains(r)) {
            let text = self.update(role, persona, &context, &updates).await?;
            if text.to_lowercase().contains("block") {
                raised.push(((*role).to_owned(), text.clone()));
            }
            self.post(role, &text).await;
            updates.push(((*role).to_owned(), text));
        }

        // Each raised blocker gets handled: the most relevant teammate replies
        // with a concrete action to resolve it — so a blocker never just hangs.
        let blockers = raised.len();
        for (owner, blocker) in &raised {
            let responder = responder_for(&blocker.to_lowercase());
            let reply = self.resolve(responder, owner, blocker, &context).await?;
            self.post(responder, &reply).await;
            updates.push((responder.to_owned(), reply));
        }

        // SM closes: highlight blockers + the focus for the day.
        let close = self.highlight(&context, &updates).await?;
        self.post("SM", &format!("📌 {}", close.trim())).await;
        Ok(blockers)
    }

    /// The SM's spoken opener: a two-sentence read on the sprint plus an explicit
    /// ask for the team to give their updates.
    async fn opener(&self, context: &str) -> Result<String, AppError> {
        let headline = self.headline().await;
        let task = format!(
            "{context}\nStatus line: {headline}\nYou are the SM opening the daily standup. In 2 \
             sentences, greet the team, give a quick read on where the sprint stands, and ask \
             everyone to share what they finished, what's next, and any blocker. Warm but brief."
        );
        self.run("SM", &task).await
    }

    /// A teammate resolves a raised blocker with a concrete action.
    async fn resolve(
        &self,
        responder: &str,
        owner: &str,
        blocker: &str,
        context: &str,
    ) -> Result<String, AppError> {
        let task = format!(
            "{context}\n{owner} raised this blocker at standup:\n\"{blocker}\"\nYou are {responder}, \
             the teammate best placed to help. In 1-2 sentences, respond directly to {owner} with \
             a concrete action to resolve it (who does what, next step). If it needs tracked work, \
             say so. Be specific and own it — no vague reassurance."
        );
        self.run(responder, &task).await
    }

    /// One-line sprint headline for the SM's opener.
    async fn headline(&self) -> String {
        let Ok(s) = self.store.load().await else {
            return if self.lang.is_vi() {
                "họp nhanh hằng ngày".to_owned()
            } else {
                "daily sync".to_owned()
            };
        };
        let done = s
            .tickets
            .iter()
            .filter(|t| {
                matches!(
                    t.status(),
                    coxagent_domain::Status::Done
                        | coxagent_domain::Status::Documented
                        | coxagent_domain::Status::Verified
                )
            })
            .count();
        let inflight = s
            .tickets
            .iter()
            .filter(|t| t.status() == coxagent_domain::Status::InProgress)
            .count();
        match (&s.sprint, self.lang.is_vi()) {
            (Some(sp), true) => format!(
                "Sprint #{} “{}” · {done} xong · {inflight} đang làm. Điểm danh cả nhóm:",
                sp.number, sp.goal
            ),
            (Some(sp), false) => format!(
                "Sprint #{} “{}” · {done} done · {inflight} in flight. Round the room:",
                sp.number, sp.goal
            ),
            (None, true) => format!("{done} đã ship · {inflight} đang làm. Điểm danh cả nhóm:"),
            (None, false) => format!("{done} shipped · {inflight} in flight. Round the room:"),
        }
    }

    /// A compact status the agents ground their updates in.
    async fn status_context(&self) -> String {
        use std::fmt::Write as _;
        let Ok(s) = self.store.load().await else {
            return String::new();
        };
        let mut out = String::from("Recent team activity:\n");
        for a in s.activity.iter().rev().take(14) {
            let _ = writeln!(
                out,
                "- {}: {}{}",
                a.agent,
                a.action,
                a.ticket
                    .as_deref()
                    .map(|t| format!(" [{t}]"))
                    .unwrap_or_default()
            );
        }
        out
    }

    /// Which roles have recent activity (so empty roles stay quiet).
    async fn active_roles(&self) -> Vec<&'static str> {
        let Ok(s) = self.store.load().await else {
            return vec!["SA", "DEV-FEATURE", "TEST"];
        };
        let recent: std::collections::HashSet<String> = s
            .activity
            .iter()
            .rev()
            .take(30)
            .map(|a| a.agent.to_uppercase())
            .collect();
        let mut roles: Vec<&'static str> = PARTICIPANTS
            .iter()
            .map(|(r, _)| *r)
            .filter(|r| recent.contains(*r))
            .collect();
        if roles.is_empty() {
            roles = vec!["SA", "DEV-FEATURE", "TEST"];
        }
        roles
    }

    async fn update(
        &self,
        role: &str,
        persona: &str,
        context: &str,
        thread: &[(String, String)],
    ) -> Result<String, AppError> {
        use std::fmt::Write as _;
        let prior = if thread.is_empty() {
            String::new()
        } else {
            let mut s = String::from("\nTeammates so far:\n");
            for (r, t) in thread {
                let _ = writeln!(s, "{r}: {t}");
            }
            s
        };
        let task = format!(
            "{context}{prior}\nYou are {role} ({persona}). Give your standup update in \
             1-2 sentences: what you finished, what you're picking up next, and — only if \
             real — one blocker (say \"BLOCKER:\" then what and who can help). Speak like a \
             teammate, first person, concrete, no filler."
        );
        self.run(role, &task).await
    }

    async fn highlight(
        &self,
        context: &str,
        thread: &[(String, String)],
    ) -> Result<String, AppError> {
        use std::fmt::Write as _;
        let mut updates = String::new();
        for (r, t) in thread {
            let _ = writeln!(updates, "{r}: {t}");
        }
        let task = format!(
            "{context}\nStandup updates:\n{updates}\nYou are the SM. In 2-3 sentences: call out \
             any blocker and who should resolve it, and name the single most important focus for \
             today. Be decisive and concrete."
        );
        self.run("SM", &task).await
    }

    async fn run(&self, role: &str, task: &str) -> Result<String, AppError> {
        let request = AgentRequest {
            role: Role::Sm,
            system_prompt: format!(
                "You are {role} at your team's daily standup. Speak plainly in the first \
                 person like a real teammate — concise, specific, honest about blockers. No \
                 preamble, no sign-off, 1-3 sentences.{}",
                self.lang.reply_directive()
            ),
            task_prompt: task.to_owned(),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(90),
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "standup engine failed for {role}: {}",
                outcome.stderr.trim()
            ))
            .into());
        }
        Ok(outcome.stdout.trim().to_owned())
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut state) = self.store.load().await {
            state.post_comment(author, body, None);
            let _ = self.store.save(&state).await;
        }
    }
}
