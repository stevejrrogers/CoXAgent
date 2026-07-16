//! `RunChatReplyUseCase` — when a human posts in the team channel, the most
//! relevant agent replies intelligently, grounded in the live project state, and
//! actually *does* the thing when the message is a request (run the architecture
//! or docs review, a standup, or kick off a team discussion). `?Sized` so the hub
//! can drive it with `dyn` adapters.

use crate::config::Language;
use crate::error::AppError;
use crate::ports::outbound::{AgentEnginePort, AgentRequest, DeployPort, StateStorePort};
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
    deploy: Option<Arc<dyn DeployPort>>,
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
            deploy: None,
        }
    }

    /// Give the chat the ability to actually deploy/run the app on request.
    #[must_use]
    pub fn with_deploy(mut self, deploy: Arc<dyn DeployPort>) -> Self {
        self.deploy = Some(deploy);
        self
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
             {persona}, replying like a sharp senior teammate — the way a good coding agent in a \
             terminal would. Answer directly and specifically, grounded in the project state above. \
             If the request is ambiguous or you need a detail to do it right, ASK a crisp \
             clarifying question instead of guessing. Keep it concise.\n\n\
             You can also DO things by appending EXACTLY ONE final line:\n\
             ACTION: arch_review                — review the architecture, file refactor tickets\n\
             ACTION: docs_review                — fill missing Wiki docs\n\
             ACTION: standup                    — run a standup\n\
             ACTION: discuss: <topic>           — kick off a team discussion\n\
             ACTION: feature: <title> :: <desc> — add a new feature to the backlog\n\
             ACTION: bug: <title> :: <desc>     — file a bug for the devs to fix\n\
             ACTION: deploy                     — build & run the app now (docker compose up)\n\
             ACTION: none                       — just talking / asking\n\n\
             IMPORTANT — confirm before acting: if the action changes the project (creating a \
             ticket, running a review/standup/discussion) and the user has NOT clearly confirmed it \
             in this thread, DO NOT act yet — reply with your proposal and a yes/no question, and \
             use `ACTION: none`. Only emit the real action once the thread shows they confirmed \
             (e.g. \"ok\", \"làm đi\", \"yes\").{}",
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
        } else if let Some(rest) = strip_kw(a, "discuss") {
            if !rest.is_empty() {
                let uc = super::RunDiscussionUseCase::new(
                    Arc::clone(&self.store),
                    Arc::clone(&self.engine),
                    self.work_dir.clone(),
                )
                .with_language(self.lang);
                let _ = uc.execute(rest).await;
            }
        } else if let Some(rest) = strip_kw(a, "feature") {
            self.file_ticket(coxagent_domain::TicketType::Feature, rest, "PO")
                .await;
        } else if let Some(rest) = strip_kw(a, "bug") {
            self.file_ticket(coxagent_domain::TicketType::Bug, rest, "TEST")
                .await;
        } else if lower.starts_with("deploy") {
            self.deploy_now().await;
        }
        Ok(())
    }

    /// Deploy/run the app on request (docker compose in the codebase), reporting
    /// the result back into the channel.
    async fn deploy_now(&self) {
        let Some(deploy) = &self.deploy else {
            let msg = if self.lang.is_vi() {
                "Mình chưa được cấp quyền deploy ở đây — cần bật deploy adapter cho project."
            } else {
                "I don't have deploy access here — a deploy adapter needs enabling for this project."
            };
            self.post("DEV-BUG", msg).await;
            return;
        };
        let starting = if self.lang.is_vi() {
            "🚀 Đang deploy (docker compose up)…"
        } else {
            "🚀 Deploying (docker compose up)…"
        };
        self.post("DEV-BUG", starting).await;
        let result = deploy.deploy(&self.work_dir).await;
        let body = match result {
            Ok(r) if r.success => {
                if self.lang.is_vi() {
                    format!("✅ Deploy xong — {}", r.summary)
                } else {
                    format!("✅ Deploy OK — {}", r.summary)
                }
            }
            Ok(r) => {
                if self.lang.is_vi() {
                    format!(
                        "❌ Deploy fail — {}. Mình sẽ tạo bug để xử lý nếu bạn muốn.",
                        r.summary
                    )
                } else {
                    format!(
                        "❌ Deploy failed — {}. I can file a bug to fix it if you want.",
                        r.summary
                    )
                }
            }
            Err(e) => {
                if self.lang.is_vi() {
                    format!("❌ Không chạy được deploy: {e}")
                } else {
                    format!("❌ Couldn't run the deploy: {e}")
                }
            }
        };
        self.post("DEV-BUG", &body).await;
    }

    /// Create a ticket from a `<title> :: <description>` payload and confirm it in
    /// the channel, so a chat request turns into tracked, actionable work.
    async fn file_ticket(&self, kind: coxagent_domain::TicketType, payload: &str, author: &str) {
        use coxagent_domain::ticket::{Complexity, Priority};
        let (title, desc) = payload
            .split_once("::")
            .map_or((payload.trim(), ""), |(t, d)| (t.trim(), d.trim()));
        if title.is_empty() {
            return;
        }
        let priority = if kind == coxagent_domain::TicketType::Bug {
            Priority::High
        } else {
            Priority::Medium
        };
        let adder = super::AddTicketUseCase::new(Arc::clone(&self.store));
        if let Ok(id) = adder
            .execute(super::AddTicketInput {
                ticket_type: kind,
                title: title.to_owned(),
                description: if desc.is_empty() {
                    format!("Requested in the team chat: {title}")
                } else {
                    desc.to_owned()
                },
                priority,
                complexity: Complexity::Medium,
                has_ui: false,
                acceptance_criteria: Vec::new(),
            })
            .await
        {
            let kind_str = if kind == coxagent_domain::TicketType::Bug {
                "bug"
            } else {
                "tính năng"
            };
            let msg = if self.lang.is_vi() {
                format!("🎫 Đã tạo {id} ({kind_str}): {title}. Team sẽ đưa vào quy trình.")
            } else {
                format!("🎫 Filed {id}: {title}. The team will pick it up.")
            };
            self.post(author, &msg).await;
        }
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
        let _ = writeln!(out, "- Current version: {}", s.current_version);
        if let Some(last) = s.history.last() {
            let _ = writeln!(out, "- Last deploy: {} ({})", last.version, last.title);
        }
        out.push_str(
            "(The app auto-deploys via docker compose after DEV each cycle; you can also deploy \
             on request with ACTION: deploy.)\n",
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

/// Strip a `<keyword>` / `<keyword>:` prefix and return the trimmed remainder,
/// or `None` when `a` isn't that directive.
fn strip_kw<'a>(a: &'a str, kw: &str) -> Option<&'a str> {
    let rest = a.strip_prefix(kw)?;
    Some(rest.trim_start_matches([':', ' ']).trim())
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
