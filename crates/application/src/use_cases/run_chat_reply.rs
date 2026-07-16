//! `RunChatReplyUseCase` — when a human posts in the team channel, the most
//! relevant agent replies intelligently, grounded in the live project state, and
//! actually *does* the thing when the message is a request (run the architecture
//! or docs review, a standup, or kick off a team discussion). `?Sized` so the hub
//! can drive it with `dyn` adapters.

use crate::config::Language;
use crate::error::AppError;
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use coxagent_domain::Role;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Runs one agent reply to a human's team-channel message.
pub struct RunChatReplyUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    token_saver: bool,
    lang: Language,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunChatReplyUseCase<S, E> {
    pub fn new(
        store: Arc<S>,
        engine: Arc<E>,
        work_dir: PathBuf,
        token_saver: bool,
        lang: Language,
    ) -> Self {
        Self {
            store,
            engine,
            work_dir,
            token_saver,
            lang,
        }
    }

    /// Reply to `user_msg`. Best-effort: engine hiccups just yield no reply.
    ///
    /// # Errors
    /// [`AppError`] only propagates from a triggered action's store failure.
    pub async fn execute(&self, user_msg: &str) -> Result<(), AppError> {
        let msg = user_msg.trim();
        if msg.is_empty() {
            return Ok(());
        }
        let persona = route_persona(&msg.to_lowercase());
        let context = self.context().await;
        let task = format!(
            "{context}\nA human teammate just wrote in the team channel:\n\"{msg}\"\n\nYou are \
             {persona}. Reply like a sharp, helpful teammate: answer directly and specifically, \
             grounded in the project state above — not generically. Keep it 1-4 sentences.\n\n\
             If (and only if) they are asking the team to DO one of these, append a final line \
             exactly one of:\nACTION: arch_review   (review the architecture, file refactor \
             tickets)\nACTION: docs_review   (fill missing Wiki docs)\nACTION: standup\n\
             ACTION: discuss: <one-line topic>   (kick off a team discussion)\nOtherwise append \
             `ACTION: none`.{}",
            self.lang.reply_directive()
        );
        let Some(raw) = self.run(persona, &task).await else {
            return Ok(());
        };
        let (reply, action) = split_action(&raw);
        if !reply.trim().is_empty() {
            self.post(persona, reply.trim()).await;
        }
        self.dispatch(&action).await
    }

    /// Execute a parsed `ACTION:` directive, if any (each posts its own output).
    async fn dispatch(&self, action: &str) -> Result<(), AppError> {
        let a = action.trim();
        let lower = a.to_lowercase();
        if lower.is_empty() || lower == "none" {
            return Ok(());
        }
        let sprint = self.sprint_number().await;
        if lower.starts_with("arch_review") {
            let uc = super::RunArchitectureAuditUseCase::new(
                Arc::clone(&self.store),
                Arc::clone(&self.engine),
                self.work_dir.clone(),
                self.token_saver,
                self.lang,
            );
            uc.execute(sprint).await?;
        } else if lower.starts_with("docs_review") {
            let uc = super::RunDocsAuditUseCase::new(
                Arc::clone(&self.store),
                Arc::clone(&self.engine),
                self.work_dir.clone(),
                self.lang,
            );
            uc.execute(sprint).await?;
        } else if lower.starts_with("standup") {
            let uc = super::RunStandupUseCase::new(
                Arc::clone(&self.store),
                Arc::clone(&self.engine),
                self.work_dir.clone(),
            )
            .with_language(self.lang);
            let _ = uc.execute().await;
        } else if let Some(topic) = a
            .strip_prefix("discuss:")
            .or_else(|| a.strip_prefix("discuss"))
        {
            let topic = topic.trim_start_matches([':', ' ']).trim();
            if !topic.is_empty() {
                let uc = super::RunDiscussionUseCase::new(
                    Arc::clone(&self.store),
                    Arc::clone(&self.engine),
                    self.work_dir.clone(),
                )
                .with_language(self.lang);
                let _ = uc.execute(topic).await;
            }
        }
        Ok(())
    }

    /// A compact, grounded status the reply agent reasons over.
    async fn context(&self) -> String {
        use coxagent_domain::{Status, TicketType};
        let _ = self.token_saver;
        let Ok(s) = self.store.load().await else {
            return String::new();
        };
        let done = s
            .tickets
            .iter()
            .filter(|t| {
                matches!(
                    t.status(),
                    Status::Done | Status::Documented | Status::Verified
                )
            })
            .count();
        let inflight = s
            .tickets
            .iter()
            .filter(|t| t.status() == Status::InProgress)
            .count();
        let open_bugs = s
            .tickets
            .iter()
            .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
            .count();
        let mut out = String::from("Project status:\n");
        if let Some(sp) = &s.sprint {
            let _ = writeln!(out, "- Sprint #{} — goal: {}", sp.number, sp.goal);
        }
        let _ = writeln!(
            out,
            "- {done} shipped · {inflight} in progress · {open_bugs} open bug(s)"
        );
        out.push_str("Recent team channel:\n");
        for c in s
            .comments
            .iter()
            .filter(|c| c.ticket.is_none())
            .rev()
            .take(8)
        {
            let body: String = c.body.chars().take(160).collect();
            let _ = writeln!(out, "- {}: {body}", c.author);
        }
        out
    }

    async fn sprint_number(&self) -> u32 {
        self.store
            .load()
            .await
            .ok()
            .and_then(|s| s.sprint.map(|sp| sp.number))
            .unwrap_or(0)
    }

    async fn run(&self, persona: &str, task: &str) -> Option<String> {
        let request = AgentRequest {
            role: Role::Sm,
            system_prompt: format!(
                "You are {persona} on an autonomous software team, chatting with a human teammate \
                 in the team channel. Be concrete, grounded, and genuinely useful — never generic \
                 filler. No preamble, no sign-off."
            ),
            task_prompt: task.to_owned(),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(120),
        };
        let outcome = self.engine.run(request).await.ok()?;
        outcome
            .succeeded()
            .then(|| outcome.stdout.trim().to_owned())
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut state) = self.store.load().await {
            state.post_comment(author, body, None);
            let _ = self.store.save(&state).await;
        }
    }
}

/// Pick which agent should answer a human message from its wording.
fn route_persona(lower: &str) -> &'static str {
    let has = |kw: &[&str]| kw.iter().any(|k| lower.contains(k));
    if has(&[
        "architecture",
        "kiến trúc",
        "refactor",
        "scale",
        "microservice",
        "design pattern",
    ]) {
        "SA"
    } else if has(&["doc", "tài liệu", "wiki", "document"]) {
        "DOCS"
    } else if has(&["bug", "deploy", "build", "lỗi", "crash", "fix"]) {
        "DEV-BUG"
    } else if has(&["feature", "tính năng", "implement", "code"]) {
        "DEV-FEATURE"
    } else if has(&[
        "priority",
        "ưu tiên",
        "backlog",
        "roadmap",
        "scope",
        "sprint goal",
    ]) {
        "PO"
    } else if has(&["test", "qa", "kiểm thử"]) {
        "TEST"
    } else if has(&["design", "ux", "giao diện"]) {
        "PD"
    } else {
        "SM"
    }
}

/// Split a trailing `ACTION: <directive>` line off the reply body.
fn split_action(raw: &str) -> (String, String) {
    for (i, line) in raw.lines().enumerate() {
        let t = line.trim();
        if let Some(rest) = t
            .strip_prefix("ACTION:")
            .or_else(|| t.strip_prefix("Action:"))
        {
            let body: Vec<&str> = raw.lines().take(i).collect();
            return (body.join("\n"), rest.trim().to_owned());
        }
    }
    (raw.to_owned(), String::new())
}
