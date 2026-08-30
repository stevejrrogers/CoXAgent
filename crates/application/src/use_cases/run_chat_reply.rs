//! `RunChatReplyUseCase` — when a human posts in the team channel, the most
//! relevant agent replies intelligently, grounded in the live project state, and
//! actually *does* the thing when the message is a request (run the architecture
//! or docs review, a standup, or kick off a team discussion). `?Sized` so the hub
//! can drive it with `dyn` adapters.

use crate::config::Language;
use crate::error::AppError;
use crate::ports::outbound::{AgentEnginePort, AgentRequest, DeployPort, StateStorePort};
use crate::state::ProjectState;
use coxagent_domain::{Role, Status};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Creates a new project from scratch: `(name, alias) -> human-readable status`.
pub type NewProjectFn = Arc<dyn Fn(String, Option<String>) -> Result<String, String> + Send + Sync>;

/// Imports an existing codebase: `(path, name, alias) -> human-readable status`.
pub type ImportProjectFn =
    Arc<dyn Fn(String, String, Option<String>) -> Result<String, String> + Send + Sync>;

/// Runs one agent reply to a human's team-channel message.
pub struct RunChatReplyUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    token_saver: bool,
    lang: Language,
    deploy: Option<Arc<dyn DeployPort>>,
    /// The published `host_port`, or `Err` when the raw config's
    /// `deploy.host_port` is present but malformed (COX-B035) — an `Err`
    /// fails the mandatory post-deploy health gate rather than being folded
    /// into "nothing configured".
    host_port_probe: Result<Option<u16>, ()>,
    /// Code host + target branch, so chat can trigger an SA merge sweep.
    forge: Option<(Arc<dyn crate::ports::outbound::ForgePort>, String, bool)>,
    context: Option<String>,
    /// Callback: create a new project from scratch. Returns a human-readable status message.
    new_project_fn: Option<NewProjectFn>,
    /// Callback: import an existing codebase. Returns a human-readable status message.
    import_project_fn: Option<ImportProjectFn>,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    /// Channel the reply posts into; `None` keeps the Scrum/discuss thread.
    reply_channel: Option<String>,
    /// The chat author's role, so a gate command typed in chat ("approve F12")
    /// obeys the same role map as the Inbox buttons. `None` = open mode (no
    /// auth), where the sole operator may do everything.
    actor_role: Option<crate::auth::AuthRole>,
}

/// Common docker-compose filenames we treat as "already has a deploy setup".
const COMPOSE_NAMES: &[&str] = &[
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

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
            host_port_probe: Ok(None),
            forge: None,
            context: None,
            new_project_fn: None,
            import_project_fn: None,
            files: None,
            reply_channel: None,
            actor_role: None,
        }
    }

    /// Carry the chat author's role so gate commands honour it.
    #[must_use]
    pub fn with_actor_role(mut self, role: Option<crate::auth::AuthRole>) -> Self {
        self.actor_role = role;
        self
    }

    /// Reply into a chat CHANNEL instead of the Scrum thread — the answer
    /// belongs where the question was asked.
    #[must_use]
    pub fn with_reply_channel(mut self, channel: Option<String>) -> Self {
        self.reply_channel = channel;
        self
    }

    /// Attach the files port so chat-triggered reviews can scan the workspace.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    #[must_use]
    pub fn with_new_project_fn(mut self, f: NewProjectFn) -> Self {
        self.new_project_fn = Some(f);
        self
    }

    #[must_use]
    pub fn with_import_project_fn(mut self, f: ImportProjectFn) -> Self {
        self.import_project_fn = Some(f);
        self
    }

    #[must_use]
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// Give the chat the ability to run an SA merge sweep over PRs into `target`.
    #[must_use]
    pub fn with_forge(
        mut self,
        forge: Arc<dyn crate::ports::outbound::ForgePort>,
        target: impl Into<String>,
        require_ci: bool,
    ) -> Self {
        self.forge = Some((forge, target.into(), require_ci));
        self
    }

    /// Give the chat the ability to actually deploy/run the app on request.
    #[must_use]
    pub fn with_deploy(mut self, deploy: Arc<dyn DeployPort>) -> Self {
        self.deploy = Some(deploy);
        self
    }

    /// The host port to publish on when scaffolding a docker setup, and to
    /// probe for the mandatory post-deploy health gate. `Err(())` means the
    /// raw config's `deploy.host_port` was present but malformed — the gate
    /// must fail rather than treat it as unconfigured (COX-B035).
    #[must_use]
    pub fn with_host_port_probe(mut self, probe: Result<Option<u16>, ()>) -> Self {
        self.host_port_probe = probe;
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
        // Bare gate commands are deterministic — handled without the model.
        if self.try_gate_command(msg).await {
            return Ok(());
        }
        let persona = route_persona(&msg.to_lowercase());
        let context = self.context().await;
        // A broad or strategic question deserves the TEAM, not one voice:
        // PO/SA/SM think in parallel, then one synthesis answers with the
        // distinct viewpoints and a single recommendation — the thing a lone
        // assistant cannot give you.
        if wants_panel(msg) {
            return self.panel_reply(msg, &context).await;
        }
        let task = format!(
            "{context}\nA human teammate just wrote in the team channel:\n\"{msg}\"\n\nYou are \
             {persona}, replying like a sharp senior teammate — the way a good coding agent in a \
             terminal would. Answer directly and specifically, grounded in the project state above \
             and in the code, which you can read.\n\n\
             When a teammate reports something broken, that IS the request. Go and look: read the \
             code for the thing they named, check the backlog above for a ticket that already \
             covers it, and come back with what you FOUND — the file and the line, whether it is \
             already filed — then file it or fix it. Never answer a problem report with a menu of \
             options for the human to pick from, and never ask permission to record a bug they \
             just told you about. Ask a question only when the answer would change what you do, \
             you could not find it in the code or the state, and you ask exactly one. Keep it \
             concise.\n\n\
             You can also DO things by appending EXACTLY ONE final line:\n\
             ACTION: new_project: <name> :: <alias>              — create a new project from scratch\n\
             ACTION: import: <path> :: <name> :: <alias>         — import an existing codebase\n\
             ACTION: arch_review                — review the architecture, file refactor tickets\n\
             ACTION: docs_review                — fill missing Wiki docs\n\
             ACTION: sa_design: <ticket-id>     — SA designs ONE ticket (runs the SA agent now)\n\
             ACTION: approve: <ticket-id>       — HUMAN gate: release a designed ticket to Ready\n\
             ACTION: verify: <ticket-id>        — HUMAN gate: render the QA verdict (Fixed→Verified)\n\
             ACTION: implement: <ticket-id>     — code the ticket NOW (DEV runs, writes code, tests)\n\
             ACTION: test: <ticket-id>          — QA tests the deployed ticket, files bugs\n\
             ACTION: standup                    — run a standup\n\
             ACTION: discuss: <topic>           — kick off a team discussion (PO+SA weigh in, SM decides)\n\
             ACTION: feature: <title> :: <desc> :: <low|medium|high> [:: sprint] — add a feature (append `:: sprint` to also commit it into the RUNNING sprint)\n\
             ACTION: bug: <title> :: <desc> :: <low|medium|high> [:: sprint]     — file a bug (`:: sprint` commits it into the running sprint)\n\
             ACTION: priority: <ticket-id> :: <low|medium|high>      — reprioritise an existing ticket\n\
             ACTION: implement: <ticket-id>    — code the ticket NOW (DEV runs, writes code, tests)\n\
             ACTION: deploy                     — build & run the app now (docker compose up)\n\
             ACTION: merge_queue                — SA merges every green open PR right now\n\
             ACTION: none                       — just talking / asking\n\n\
             For `implement:`, only use it when the human explicitly says \"code it\", \"implement\", \
             \"build it\", \"do it\", \"làm đi\" — never auto-implement. The DEV runs and writes real code.\n\
             For a real decision that needs the team (should we build X? which approach?), prefer \
             `discuss:` so PO and SA debate and the SM decides. Set a sensible priority when you \
             file work.\n\
             CONFIRM only what is expensive or hard to undo — `new_project`, `import`, `deploy`, \
             `merge_queue`, `implement`, and the review/standup/discussion runs. For those, if the \
             thread does not already show a clear yes, propose it and use `ACTION: none`.\n\
             Recording what the user just told you is NOT in that class: a reported bug gets \
             `ACTION: bug:` and a requested feature gets `ACTION: feature:` in the same reply that \
             reports what you found. Asking \"shall I file it?\" wastes their turn.\n\
             A short affirmative anywhere in the thread — \"ok\", \"uhm\", \"ừ\", \"đi\", \"làm đi\", \
             \"ưu tiên fix\", \"yes\", \"go\" — IS the confirmation of whatever you last proposed. \
             Act on it. Asking the same question again after the human already said yes is the \
             worst thing you can do here: they answered, and the work still has not started.\n\
             The ACTION line is your ONLY hand on the board. Nothing you merely SAY happens: if \
             you claim you filed, created, or queued something and this reply does not end with \
             the matching ACTION line, you have lied to the team. \"tạo ticket\", \"thêm vào \
             sprint\", \"file it\" in ANY language means: end THIS reply with the ACTION line.{}",
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
        // Expensive or hard to undo: deploying, merging the queue, creating or
        // importing a project, or setting a developer to work. The prompt asks
        // for confirmation first; a model answering a bug report reached for
        // `deploy` anyway, so the rule is enforced here where it cannot be
        // talked out of.
        if NEEDS_YES.iter().any(|k| lower.starts_with(k)) && !self.thread_has_confirmation().await {
            let msg = format!(
                "I held off on `{a}` — that one changes the project, and I do not see a yes in \
                 this thread yet. Say the word and I will run it."
            );
            self.post("SM", &msg).await;
            return Ok(());
        }
        let sprint = self.sprint_number().await;
        if let Some(rest) = strip_kw(a, "new_project") {
            self.new_project(rest).await;
        } else if let Some(rest) = strip_kw(a, "import") {
            self.import_project(rest).await;
        } else if lower.starts_with("arch_review") {
            let uc = super::RunArchitectureAuditUseCase::new(
                Arc::clone(&self.store),
                Arc::clone(&self.engine),
                self.work_dir.clone(),
                self.token_saver,
                self.lang,
            )
            .with_files(self.files.clone());
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
        } else if let Some(rest) = strip_kw(a, "sa_design") {
            self.design_ticket(rest.trim()).await;
        } else if let Some(rest) = strip_kw(a, "feature") {
            self.file_ticket(coxagent_domain::TicketType::Feature, rest, "PO")
                .await;
        } else if let Some(rest) = strip_kw(a, "bug") {
            self.file_ticket(coxagent_domain::TicketType::Bug, rest, "TEST")
                .await;
        } else if let Some(rest) = strip_kw(a, "implement") {
            self.implement_ticket(rest).await;
        } else if let Some(rest) = strip_kw(a, "test") {
            self.test_ticket(rest.trim()).await;
        } else if let Some(rest) = strip_kw(a, "priority") {
            self.reprioritize(rest).await;
        } else if let Some(rest) = strip_kw(a, "approve") {
            self.human_gate_action(rest, coxagent_domain::Status::Ready)
                .await;
        } else if let Some(rest) = strip_kw(a, "verify") {
            self.human_gate_action(rest, coxagent_domain::Status::Verified)
                .await;
        } else if lower.starts_with("deploy") {
            self.deploy_now().await;
        } else if lower.starts_with("merge_queue") || lower.starts_with("merge queue") {
            if let Some((forge, target, require_ci)) = &self.forge {
                let _ = crate::use_cases::merge_sweep(
                    forge.as_ref(),
                    self.store.as_ref(),
                    target,
                    self.lang.is_vi(),
                    *require_ci,
                )
                .await;
            }
        }
        Ok(())
    }

    /// Handle a bare gate command ("approve COX-F023", "verify B031") without
    /// a model in the loop — the engine path once answered it "does not exist"
    /// because the prompt's bounded backlog omitted the ticket. Returns whether
    /// the message WAS a gate command (and was handled). Role-gated the same way
    /// as the Inbox buttons: approve is BA/PO, verify is QA; open mode (no role)
    /// is the operator and may do both.
    async fn try_gate_command(&self, msg: &str) -> bool {
        let lower = msg.to_lowercase();
        for (kw, to) in [
            ("approve", coxagent_domain::Status::Ready),
            ("verify", coxagent_domain::Status::Verified),
        ] {
            let Some(rest) = lower.strip_prefix(kw) else {
                continue;
            };
            let id = rest.trim_start_matches([':', ' ']).trim();
            let orig = msg[msg.len() - id.len()..].trim();
            if id.is_empty() || id.contains(' ') || !id.contains('-') {
                continue;
            }
            let verify = to == coxagent_domain::Status::Verified;
            let allowed = self.actor_role.map_or(true, |r| {
                if verify {
                    r.can_verify()
                } else {
                    r.can_approve_ready()
                }
            });
            if allowed {
                self.human_gate_action(orig, to).await;
            } else {
                let who = if verify { "QA/Tester" } else { "BA/PO" };
                self.post(
                    "SYSTEM",
                    &format!(
                        "⛔ Only {who} may {kw} a ticket — your role can view but not take this \
                         decision."
                    ),
                )
                .await;
            }
            return true;
        }
        false
    }

    /// A human gate decision typed in chat: "approve F012" moves a designed
    /// ticket to Ready, "verify B031" renders the QA verdict — the same moves
    /// the Inbox buttons make, executed with the chat user's authority
    /// (Role::User; the domain transition table decides legality).
    async fn human_gate_action(&self, rest: &str, to: coxagent_domain::Status) {
        use coxagent_domain::TicketId;
        let tid_s = rest.trim();
        let Ok(tid) = TicketId::new(tid_s) else {
            let msg = if self.lang.is_vi() {
                format!("{tid_s} không phải ticket ID hợp lệ.")
            } else {
                format!("{tid_s} is not a valid ticket ID.")
            };
            self.post("SYSTEM", &msg).await;
            return;
        };
        let label = format!("{to:?}").to_lowercase();
        let result = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            let t = s
                .ticket_mut(&tid)
                .ok_or_else(|| crate::PortError::Corrupt(format!("no ticket {tid}")))?;
            t.transition_to(coxagent_domain::Role::User, to)
                .map_err(|e| crate::PortError::Corrupt(e.to_string()))?;
            // Reaching Verified MEANS the QA verdict was rendered; the burn-down
            // (CXA-F032 AC#2) only counts bugs carrying their own REGRESSION
            // TEST PASS record, so the human verdict writes the same provenance
            // the agent TEST path has written since F022.
            if to == coxagent_domain::Status::Verified {
                super::run_test::record_human_verify_evidence(s, &tid.to_string());
                // Goal-line outcome ledger (CXA-F228): a human verdict is a
                // delivered outcome like the agent path's.
                s.record_verified_outcome(&tid.to_string());
            }
            s.log_activity(
                "USER",
                &format!("chat-approved to {label}"),
                Some(tid.to_string()),
            );
            Ok(())
        })
        .await;
        let msg = match result {
            Ok(()) => {
                if self.lang.is_vi() {
                    format!("✅ {tid} → {label}.")
                } else {
                    format!("✅ {tid} moved to {label}.")
                }
            }
            Err(e) => format!("⚠️ {tid}: {e}"),
        };
        self.post("SYSTEM", &msg).await;
    }

    /// Run the DEV agent for this specific ticket. Uses the engine directly
    /// to code a single ticket (the chat-requested implementation).
    async fn implement_ticket(&self, rest: &str) {
        use coxagent_domain::TicketId;
        let tid_s = rest.trim();
        let Ok(tid) = TicketId::new(tid_s) else {
            let msg = if self.lang.is_vi() {
                format!("{tid_s} không phải ticket ID hợp lệ.")
            } else {
                format!("{tid_s} is not a valid ticket ID.")
            };
            self.post("DEV-BUG", &msg).await;
            return;
        };
        // Check the ticket exists and has a design
        let Ok(state) = self.store.load().await else {
            return;
        };
        let Some(ticket) = state.ticket(&tid) else {
            let msg = if self.lang.is_vi() {
                format!("❌ Ticket {tid} không tồn tại.")
            } else {
                format!("❌ Ticket {tid} does not exist.")
            };
            self.post("DEV-BUG", &msg).await;
            return;
        };
        let announce = if self.lang.is_vi() {
            format!("🔨 Đang code ticket {tid}... (DEV đang làm việc, đợi chút)")
        } else {
            format!("🔨 Implementing ticket {tid}... (DEV is working, hold tight)")
        };
        self.post("DEV-FEATURE", &announce).await;

        // Build the DEV prompt manually — same as RunDevUseCase but without the
        // full state-machine cycle (claim/release handled inline).
        let memory = crate::prompts::team_memory_block(&state.decisions, &state.lessons);
        let title = ticket.title().to_owned();
        let brief = super::run_dev::ticket_brief(Some(ticket));
        let fp = crate::prompts::focus_block(
            self.files.as_deref(),
            &self.work_dir,
            &format!(
                "{title} {}",
                ticket
                    .design()
                    .technical
                    .as_ref()
                    .map_or("", |d| d.approach.as_str())
            ),
        )
        .await;
        let rp =
            crate::prompts::repo_map_block(self.files.as_deref(), &self.work_dir, self.token_saver)
                .await;

        let request = crate::ports::outbound::AgentRequest {
            role: coxagent_domain::Role::DevFeature,
            system_prompt: crate::prompts::system_prompt(crate::prompts::DEV),
            task_prompt: format!(
                "Ticket {tid}: {title}\n{brief}\nImplement it now.{fp}{rp}{memory}"
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(3600),
            escalation_level: 0,
            label: Some(tid.to_string()),
        };
        match self.engine.run(request).await {
            Ok(outcome) if outcome.succeeded() => {
                // Mark the ticket as done if possible
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    if let Some(t) = s.ticket_mut(&tid) {
                        let _ = t.transition_to(
                            coxagent_domain::Role::DevFeature,
                            coxagent_domain::Status::Done,
                        );
                    }
                    Ok(())
                })
                .await;
                let done = if self.lang.is_vi() {
                    format!(
                        "✅ Đã code xong ticket {tid}! DEV đã implement. Build & tests: {}",
                        outcome.stdout.lines().last().unwrap_or("done")
                    )
                } else {
                    format!(
                        "✅ Ticket {tid} implemented! DEV coded it. {}",
                        outcome.stdout.lines().last().unwrap_or("done")
                    )
                };
                self.post("DEV-FEATURE", &done).await;
            }
            Ok(outcome) => {
                let msg = format!(
                    "❌ DEV failed on {tid}: {}",
                    outcome.stderr.lines().last().unwrap_or("unknown error")
                );
                self.post("DEV-BUG", &msg).await;
            }
            Err(e) => {
                let msg = format!("❌ DEV engine error on {tid}: {e}");
                self.post("DEV-BUG", &msg).await;
            }
        }
    }

    /// SA designs a single ticket from chat.
    async fn design_ticket(&self, rest: &str) {
        use coxagent_domain::TicketId;
        let Ok(tid) = TicketId::new(rest) else { return };
        let state = self.store.load().await.ok();
        let title = state
            .as_ref()
            .and_then(|s| s.ticket(&tid).map(|t| t.title().to_owned()))
            .unwrap_or_default();
        let _has_design = state
            .as_ref()
            .and_then(|s| s.ticket(&tid))
            .is_some_and(|t| t.design().technical.is_some());
        let memory = state.as_ref().map_or(String::new(), |s| {
            crate::prompts::team_memory_block(&s.decisions, &s.lessons)
        });
        let announce = if self.lang.is_vi() {
            format!("🎨 SA đang design ticket {tid}: {title}...")
        } else {
            format!("🎨 SA designing ticket {tid}: {title}...")
        };
        self.post("SA", &announce).await;

        let fp = crate::prompts::focus_block(self.files.as_deref(), &self.work_dir, &title).await;
        let rp =
            crate::prompts::repo_map_block(self.files.as_deref(), &self.work_dir, self.token_saver)
                .await;
        let request = crate::ports::outbound::AgentRequest {
            role: coxagent_domain::Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: format!("Design feature {tid}: {title}{fp}{rp}{memory}"),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(1200),
            escalation_level: 0,
            label: Some(tid.to_string()),
        };
        match self.engine.run(request).await {
            Ok(o) if o.succeeded() => {
                // Parse SA output and attach design
                if let Some(ref _s) = state {
                    if let Ok(design) = serde_json::from_str::<serde_json::Value>(&o.stdout) {
                        let td = coxagent_domain::TechnicalDesign {
                            alternatives: String::new(),
                            approach: design
                                .get("approach")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned(),
                            files: design
                                .get("files")
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            api_contract: design
                                .get("api_contract")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned(),
                            data_changes: design
                                .get("data_changes")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned(),
                            test_plan: design
                                .get("test_plan")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned(),
                        };
                        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                            if let Some(t) = s.ticket_mut(&tid) {
                                let _ =
                                    t.set_technical_design(coxagent_domain::Role::Sa, td.clone());
                            }
                            Ok(())
                        })
                        .await;
                    }
                }
                let done = if self.lang.is_vi() {
                    format!("✅ SA đã design xong ticket {tid}. Có thể dùng `ACTION: implement: {tid}` để code.")
                } else {
                    format!(
                        "✅ SA designed ticket {tid}. Use `ACTION: implement: {tid}` to code it."
                    )
                };
                self.post("SA", &done).await;
            }
            _ => {
                self.post("SA", &format!("❌ SA failed on {tid}")).await;
            }
        }
    }

    /// QA tests a deployed ticket from chat.
    async fn test_ticket(&self, rest: &str) {
        use coxagent_domain::TicketId;
        let Ok(tid) = TicketId::new(rest) else { return };
        self.post("TEST", &format!("🧪 QA testing ticket {tid}..."))
            .await;
        let state = self.store.load().await.ok();
        let shipped = state
            .as_ref()
            .map_or(String::new(), crate::use_cases::run_test::shipped_block);
        let memory = state.as_ref().map_or(String::new(), |s| {
            crate::prompts::team_memory_block(&s.decisions, &s.lessons)
        });
        let request = crate::ports::outbound::AgentRequest {
            role: coxagent_domain::Role::Test,
            system_prompt: crate::prompts::system_prompt(crate::prompts::TEST),
            task_prompt: format!(
                "Test ticket {tid} and report bugs. Focus on this ticket's acceptance criteria first, then risk-based testing.{shipped}{memory}"
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(1800),
            escalation_level: 0,
            label: Some(tid.to_string()),
        };
        match self.engine.run(request).await {
            Ok(o) if o.succeeded() => {
                if let Ok(bugs) = serde_json::from_str::<Vec<serde_json::Value>>(&o.stdout) {
                    let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
                    let mut filed = 0;
                    for b in bugs {
                        let title = b.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let desc = b.get("description").and_then(|v| v.as_str()).unwrap_or("");
                        let prio_str = b
                            .get("priority")
                            .and_then(|v| v.as_str())
                            .unwrap_or("medium");
                        let cx_str = b
                            .get("complexity")
                            .and_then(|v| v.as_str())
                            .unwrap_or("small");
                        if title.is_empty() {
                            continue;
                        }
                        let _ = adder
                            .execute(crate::use_cases::AddTicketInput {
                                ticket_type: coxagent_domain::TicketType::Bug,
                                title: title.to_owned(),
                                description: desc.to_owned(),
                                priority: parse_priority(prio_str)
                                    .unwrap_or(coxagent_domain::Priority::Medium),
                                complexity: match cx_str {
                                    "large" => coxagent_domain::Complexity::Large,
                                    "medium" => coxagent_domain::Complexity::Medium,
                                    _ => coxagent_domain::Complexity::Small,
                                },
                                has_ui: b
                                    .get("has_ui")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false),
                                acceptance_criteria: vec![],
                                goal: None,
                            })
                            .await
                            .ok();
                        filed += 1;
                    }
                    let done = if filed > 0 {
                        format!("🧪 QA tested {tid} — filed {filed} bug(s)")
                    } else {
                        format!("✅ QA tested {tid} — no bugs found!")
                    };
                    self.post("TEST", &done).await;
                }
            }
            _ => {
                self.post("TEST", &format!("❌ TEST failed on {tid}")).await;
            }
        }
    }

    /// Create a new project from scratch via chat: `<name> :: <alias>`
    async fn new_project(&self, rest: &str) {
        let parts: Vec<&str> = rest.splitn(2, "::").map(str::trim).collect();
        let name = parts.first().copied().unwrap_or_default().to_owned();
        let alias = parts
            .get(1)
            .copied()
            .map(str::to_owned)
            .filter(|s| !s.is_empty());
        if name.is_empty() {
            self.post("SM", "Usage: new_project: Project Name :: ALIAS")
                .await;
            return;
        }
        self.post("SM", &format!("🆕 Creating project '{name}'..."))
            .await;
        let result = match &self.new_project_fn {
            Some(f) => f(name, alias),
            None => Err("Project creation not wired (run coxagent hub directly)".into()),
        };
        match result {
            Ok(msg) => {
                self.post("SM", &msg).await;
            }
            Err(e) => {
                self.post("SM", &format!("❌ Failed: {e}")).await;
            }
        }
    }

    async fn import_project(&self, rest: &str) {
        let parts: Vec<&str> = rest.splitn(3, "::").map(str::trim).collect();
        let path = parts.first().copied().unwrap_or_default().to_owned();
        let name = parts.get(1).copied().unwrap_or_default().to_owned();
        let alias = parts
            .get(2)
            .copied()
            .map(str::to_owned)
            .filter(|s| !s.is_empty());
        if path.is_empty() || name.is_empty() {
            self.post(
                "SM",
                "Usage: import: /path/to/codebase :: Project Name :: ALIAS",
            )
            .await;
            return;
        }
        self.post("SM", &format!("📂 Importing '{name}' from {path}..."))
            .await;
        let result = match &self.import_project_fn {
            Some(f) => f(path, name, alias),
            None => Err("Project import not wired (run coxagent hub directly)".into()),
        };
        match result {
            Ok(msg) => {
                self.post("SM", &msg).await;
            }
            Err(e) => {
                self.post("SM", &format!("❌ Failed: {e}")).await;
            }
        }
    }

    /// Reprioritise an existing ticket from chat: `<id> :: <level>` (or space).
    async fn reprioritize(&self, rest: &str) {
        use coxagent_domain::{Role, TicketId};
        let (id_s, lvl_s) = rest
            .split_once("::")
            .or_else(|| rest.split_once(char::is_whitespace))
            .unwrap_or((rest, ""));
        let (Some(prio), Ok(tid)) = (parse_priority(lvl_s), TicketId::new(id_s.trim())) else {
            return;
        };
        let mut done = false;
        let res = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if let Some(t) = s.ticket_mut(&tid) {
                done = t.set_priority(Role::Po, prio).is_ok();
            }
            Ok(())
        })
        .await;
        if res.is_ok() && done {
            let msg = if self.lang.is_vi() {
                format!("⬆️ Đã đổi ưu tiên {tid} → {prio:?}.")
            } else {
                format!("⬆️ Set {tid} priority to {prio:?}.")
            };
            self.post("PO", &msg).await;
        }
    }

    /// Mandatory post-deploy health gate (COX-B004/COX-B009), fail-closed on
    /// a malformed `host_port` (COX-B035): a corrupt config must not be
    /// treated as "nothing to probe", which would report a dead deploy as
    /// healthy.
    async fn deploy_health_gate(&self, deploy: &Arc<dyn DeployPort>) -> bool {
        match self.host_port_probe {
            Ok(port) => crate::ports::outbound::verify_deploy_health(deploy, port).await,
            Err(()) => false,
        }
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
        // Make sure the Docker daemon is up — start it if it's off, and only ask
        // the human to help when we genuinely can't bring it up.
        if !matches!(deploy.ensure_daemon().await, Ok(true)) {
            let msg = if self.lang.is_vi() {
                "🐳 Docker daemon đang tắt và mình bật lên không được. Bạn mở Docker Desktop \
                 giúp rồi nhắn \"deploy lại\" nhé."
            } else {
                "🐳 The Docker daemon is off and I couldn't start it. Please open Docker \
                 Desktop, then say \"deploy again\"."
            };
            self.post("DEV-BUG", msg).await;
            return;
        }

        // Greenfield / no infra? Scaffold a Dockerfile + compose so "deploy" just
        // works locally, then run it.
        let has_compose = COMPOSE_NAMES.iter().any(|f| self.work_dir.join(f).exists());
        if !has_compose {
            let scaffolding = if self.lang.is_vi() {
                "🛠️ Chưa có docker setup — mình đang tạo Dockerfile + docker-compose để chạy local…"
            } else {
                "🛠️ No docker setup yet — scaffolding a Dockerfile + docker-compose to run locally…"
            };
            self.post("DEV-FEATURE", scaffolding).await;
            self.scaffold_docker().await;
        }

        let starting = if self.lang.is_vi() {
            "🚀 Đang deploy (docker compose up)…"
        } else {
            "🚀 Deploying (docker compose up)…"
        };
        self.post("DEV-BUG", starting).await;
        let result = deploy.deploy(&self.work_dir).await;
        let body = match result {
            // Mandatory health gate (COX-B004/COX-B009): a compose exit-0 only
            // proves the containers started, not that the app inside bound
            // its port — probe before telling the human it's up.
            Ok(r) if r.success && self.deploy_health_gate(deploy).await => {
                if self.lang.is_vi() {
                    format!("✅ Deploy xong — {}", r.summary)
                } else {
                    format!("✅ Deploy OK — {}", r.summary)
                }
            }
            Ok(r) if r.success => {
                if self.lang.is_vi() {
                    format!(
                        "❌ Deploy fail — {} (container chạy nhưng app không mở port — health \
                         check fail). Mình sẽ tạo bug để xử lý nếu bạn muốn.",
                        r.summary
                    )
                } else {
                    format!(
                        "❌ Deploy failed — {} (containers started but the app never bound its \
                         port — health check failed). I can file a bug to fix it if you want.",
                        r.summary
                    )
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
        let (title, desc, prio, to_sprint) = parse_ticket_payload(payload);
        if title.is_empty() {
            return;
        }
        let (title, desc) = (title.as_str(), desc.as_str());
        let priority = prio.unwrap_or(if kind == coxagent_domain::TicketType::Bug {
            Priority::High
        } else {
            Priority::Medium
        });
        // A ticket filed from chat used to land as a title, a sentence, and no
        // acceptance criteria — nothing a developer could build against or a
        // tester could check. Put it through the same refinement the BA uses,
        // so a bug arrives with its repro, its actual/expected, and criteria.
        // The raw report is kept as the fallback: a filed ticket beats none.
        let raw = if desc.is_empty() {
            format!("{title} (reported in the team chat)")
        } else {
            format!("{title}\n\n{desc}")
        };
        let refined = super::RefineTicketUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .execute(
            &if kind == coxagent_domain::TicketType::Bug {
                format!(
                    "Bug report from the team chat. Write it up as a bug ticket: state the \
                     problem, the steps to reproduce it, the actual result and the expected \
                     result, and give acceptance criteria a tester can check \
                     mechanically.\n\n{raw}"
                )
            } else {
                raw.clone()
            },
            &self.context().await,
        )
        .await
        .ok();
        let adder = super::AddTicketUseCase::new(Arc::clone(&self.store));
        if let Ok(id) = adder
            .execute(super::AddTicketInput {
                ticket_type: kind,
                title: title.to_owned(),
                description: refined.as_ref().map_or_else(
                    || {
                        if desc.is_empty() {
                            format!("Requested in the team chat: {title}")
                        } else {
                            desc.to_owned()
                        }
                    },
                    |r| r.description.clone(),
                ),
                priority,
                complexity: Complexity::Medium,
                has_ui: refined.as_ref().is_some_and(|r| r.has_ui),
                acceptance_criteria: refined
                    .as_ref()
                    .map(|r| r.acceptance_criteria.clone())
                    .unwrap_or_default(),
                goal: None,
            })
            .await
        {
            let committed = if to_sprint {
                crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    crate::sprint::commit_ticket(s, &id);
                    s.log_activity("SM", "committed into the running sprint", Some(id.to_string()));
                    Ok(())
                })
                .await
                .is_ok()
            } else {
                false
            };
            let kind_str = if kind == coxagent_domain::TicketType::Bug {
                "bug"
            } else {
                "tính năng"
            };
            let msg = if self.lang.is_vi() {
                if committed {
                    format!("🎫 Đã tạo {id} ({kind_str}): {title} — và đã đưa vào sprint đang chạy.")
                } else {
                    format!("🎫 Đã tạo {id} ({kind_str}): {title}. Team sẽ đưa vào quy trình.")
                }
            } else if committed {
                format!("🎫 Filed {id}: {title} — committed into the running sprint.")
            } else {
                format!("🎫 Filed {id}: {title}. The team will pick it up.")
            };
            self.post(author, &msg).await;
        }
    }

    /// Have a DEV agent inspect the codebase and write a minimal, working
    /// Dockerfile + docker-compose so the app can run locally. Best-effort.
    async fn scaffold_docker(&self) {
        let port = self.host_port_probe.unwrap_or_default().unwrap_or(8080);
        let task = format!(
            "The project in the working directory has NO docker setup. Inspect the code — detect \
             the language, how it builds, and its entrypoint/served port — then CREATE a minimal \
             but WORKING `Dockerfile` and `docker-compose.yml` in the working directory that build \
             and run the app locally. Publish it on host port {port} (map \"{port}:<container \
             port>\" from an env var defaulting to {port}). Include any obvious dependency service \
             only if the code clearly needs it. Write the files, then print a one-line summary."
        );
        let request = AgentRequest {
            role: Role::DevFeature,
            system_prompt: crate::prompts::system_prompt(crate::prompts::DEV),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(600),
            escalation_level: 0,
            label: None,
        };
        let _ = self.engine.run(request).await;
    }

    /// A compact, grounded status the reply agent reasons over.
    async fn context(&self) -> String {
        use coxagent_domain::TicketType;
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
        let mut out = String::new();

        // ── Project context (goal, stack, scope, constraints) ──
        if let Some(ref ctx) = self.context {
            if !ctx.trim().is_empty() {
                out.push_str("Project context (goal, stack, scope, constraints):\n");
                out.push_str(ctx);
                out.push_str("\n\n");
            }
        }

        out.push_str("Project status:\n");
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
        push_backlog_and_health(&s, &mut out);
        out.push_str("Recent team channel (oldest first — this is the conversation you are in):\n");
        let recent: Vec<_> = s
            .comments
            .iter()
            .filter(|c| c.ticket.is_none())
            .rev()
            .take(16)
            .collect();
        for c in recent.into_iter().rev() {
            let body: String = c.body.chars().take(400).collect();
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

    /// Team panel: three role perspectives in parallel, one synthesis.
    async fn panel_reply(&self, msg: &str, context: &str) -> Result<(), AppError> {
        let view = |role: &str, lens: &str| {
            format!(
                "{context}\nA human teammate wrote in the team channel:\n\"{msg}\"\n\nYou are {role}.                  Give YOUR take through the {lens} lens, grounded in the project state above:                  your recommendation, the strongest reason for it, and the one risk the others                  will miss. At most 110 words, no preamble.{}",
                self.lang.reply_directive()
            )
        };
        let (po_task, sa_task, scrum_task) = (
            view("PO (product owner)", "value & priority"),
            view("SA (architect)", "technical feasibility & design"),
            view("SM (scrum master)", "process, risk & sequencing"),
        );
        let (po, sa, sm) = tokio::join!(
            self.run("PO", &po_task),
            self.run("SA", &sa_task),
            self.run("SM", &scrum_task),
        );
        let mut takes = String::new();
        for (who, t) in [("PO", &po), ("SA", &sa), ("SM", &sm)] {
            if let Some(t) = t {
                let _ = writeln!(takes, "{who} said:\n{t}\n");
            }
        }
        if takes.trim().is_empty() {
            // Every perspective failed — fall back to the single-voice path so
            // the human still gets an answer.
            let solo = view("SM", "pragmatic");
            let Some(raw) = self.run("SM", &solo).await else {
                return Ok(());
            };
            self.post("SM", raw.trim()).await;
            return Ok(());
        }
        let synth = format!(
            "{context}\nA human teammate wrote in the team channel:\n\"{msg}\"\n\nThree teammates              answered from different angles:\n{takes}\nYou are the SM. Write ONE team reply for the              human: open with the team's recommendation in one sentence, then the strongest points              from each teammate WITH attribution (\"PO thinks… SA warns… \"), keep real              disagreements visible instead of averaging them away, and close with the next concrete              step. Under 220 words.\n\nYou may also append EXACTLY ONE final line with an action,              same rules as always:\nACTION: feature: <title> :: <desc> :: <low|medium|high> [:: sprint]\n             ACTION: bug: <title> :: <desc> :: <low|medium|high> [:: sprint]\nACTION: discuss: <topic>\n             ACTION: none\nThe ACTION line is your ONLY hand on the board — claiming you filed              something without it is a lie. A request to create/queue work in ANY language              (\"tạo ticket\", \"thêm vào sprint\") means: end with the ACTION line; append              `:: sprint` when they want it in the running sprint.{}",
            self.lang.reply_directive()
        );
        let Some(raw) = self.run("TEAM", &synth).await else {
            return Ok(());
        };
        let (reply, action) = split_action(&raw);
        if !reply.trim().is_empty() {
            self.post("TEAM", reply.trim()).await;
        }
        self.dispatch(&action).await
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
            // Reading the code before answering takes longer than 2 minutes.
            timeout: Duration::from_secs(600),
            // A person is waiting on this: it is the wrong place for the
            // cheapest model. The first answers this produced were off-topic
            // and reached for `deploy` on a bug report.
            escalation_level: 1,
            label: None,
        };
        let outcome = match self.engine.run(request).await {
            Ok(o) => o,
            Err(e) => {
                // A transport-level error (engine busy, spawn failure) used to
                // return None silently — the person clicked send and watched
                // nothing happen at all. Same rule as below: the failure they
                // cannot see is worse than the failure they can.
                let why: String = e.to_string().chars().take(200).collect();
                tracing::warn!("chat reply engine error: {why}");
                self.post(
                    "SYSTEM",
                    &format!("⚠️ I couldn't answer that: {why}. Try again in a moment."),
                )
                .await;
                return None;
            }
        };
        if !outcome.succeeded() {
            // Silence is the worst reply. The person clicked send and watched
            // nothing happen — the failure they cannot see is worse than the
            // failure they can.
            let why: String = outcome.failure_detail().chars().take(200).collect();
            tracing::warn!("chat reply failed: {why}");
            self.post(
                "SYSTEM",
                &format!("⚠️ I could not answer that just now — the agent run failed ({why}). Say it again and I will retry."),
            )
            .await;
            return None;
        }
        Some(outcome.stdout.trim().to_owned())
    }

    /// Whether the thread already shows the human agreeing to something. Used
    /// to gate the expensive actions in code rather than trusting the model to
    /// respect the same rule in prose — it did not.
    async fn thread_has_confirmation(&self) -> bool {
        const YES: &[&str] = &[
            "ok",
            "oke",
            "okay",
            "uhm",
            "ừ",
            "ù",
            "đi",
            "làm đi",
            "lam di",
            "yes",
            "yep",
            "go",
            "chơi",
            "duyệt",
            "approve",
            "ưu tiên",
            "triển",
        ];
        let Ok(state) = self.store.load().await else {
            return false;
        };
        state
            .comments
            .iter()
            .rev()
            .filter(|c| c.author == "USER" || c.author == "root")
            .take(3)
            .any(|c| {
                let b = c.body.trim().to_lowercase();
                YES.iter()
                    .any(|y| b == *y || b.starts_with(&format!("{y} ")) || b.contains(*y))
            })
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut state) = self.store.load().await {
            match &self.reply_channel {
                // A channel is a conversation, not a report. Short answers
                // stay in-channel; a deep investigation goes to Scrum where
                // that content lives, with a two-line pointer in the channel.
                Some(ch) if body.chars().count() > 500 => {
                    state.post_comment(author, body, None);
                    let head: String = body.chars().take(180).collect();
                    let ptr = format!("{head}… — chi tiết đầy đủ bên tab Scrum 📋");
                    state.post_chat_in(author, &ptr, ch, Vec::new());
                }
                Some(ch) => state.post_chat_in(author, body, ch, Vec::new()),
                None => state.post_comment(author, body, None),
            }
            let _ = self.store.save(&state).await;
        }
    }
}

/// Actions that change the project in ways a person should agree to first.
const NEEDS_YES: &[&str] = &[
    "deploy",
    "merge_queue",
    "merge queue",
    "implement",
    "new_project",
    "import",
];

/// Appends the work-surface part of the agent briefing — open backlog, team
/// decisions, planned sprints, on-hold tickets, engine health — to `out`.
/// Split out of [`RunChatReplyUseCase::context`] so each half stays reviewable;
/// the agent must see the plan and the blockers to talk about them.
fn push_backlog_and_health(s: &ProjectState, out: &mut String) {
    use coxagent_domain::Status;
    // Open backlog so the agent knows what's there and can avoid duplicates.
    let pending: Vec<_> = s
        .tickets
        .iter()
        .filter(|t| matches!(t.status(), Status::Pending | Status::Ready | Status::Open))
        .take(10)
        .collect();
    if !pending.is_empty() {
        out.push_str("Open tickets (don't file duplicates):\n");
        for t in pending {
            let _ = writeln!(
                out,
                "- {} [{:?}] {} ({:?})",
                t.id(),
                t.ticket_type(),
                t.title(),
                t.priority()
            );
        }
    }
    if !s.decisions.is_empty() {
        out.push_str("Team decisions/conventions (honour these):\n");
        for d in s.decisions.iter().rev().take(6).rev() {
            let _ = writeln!(out, "- {d}");
        }
    }
    // Planned sprints + parked work — the human plans here; the agent must
    // see the plan to talk about it.
    if !s.sprint_queue.is_empty() {
        out.push_str("Planned sprints (run in this order after the current one):\n");
        for (i, q) in s.sprint_queue.iter().enumerate() {
            let _ = writeln!(
                out,
                "- #{} {} ({} ticket(s))",
                i + 1,
                q.goal,
                q.tickets.len()
            );
        }
    }
    let held: Vec<String> = s
        .tickets
        .iter()
        .filter(|t| t.status() == Status::OnHold)
        .map(|t| {
            let why = s
                .hold_reasons
                .get(&t.id().to_string())
                .cloned()
                .unwrap_or_default();
            format!("{} ({why})", t.id())
        })
        .collect();
    if !held.is_empty() {
        let _ = writeln!(
            out,
            "On hold (waiting on the outside world): {}",
            held.join(", ")
        );
    }
    // Engine health — so "why is BA slow" gets a real answer.
    let sick: Vec<String> = s
        .role_health
        .iter()
        .filter(|(_, h)| h.errors > 0)
        .map(|(r, h)| format!("{r}: {} error(s), {} timeout(s)", h.errors, h.timeouts))
        .collect();
    if !sick.is_empty() {
        let _ = writeln!(out, "Engine health: {}", sick.join(" · "));
    }
}

/// Wording that means several perspectives beat one voice: broad, strategic,
/// or comparative questions, where three views beat one.
const PANEL_CUES: &[&str] = &[
    "nên ",
    "hướng",
    "roadmap",
    "chiến lược",
    "strategy",
    "should we",
    "approach",
    "so sánh",
    "compare",
    "ý kiến",
    "opinions",
    "đánh giá",
    "thiết kế thế nào",
    "architecture",
    "plan for",
    "kế hoạch",
    "cả team",
    "@team",
    "team nghĩ",
];

/// Pick which agent should answer a human message from its wording.
/// Should the whole panel answer instead of one persona? Broad, strategic,
/// or comparative questions — where three perspectives beat one voice.
fn wants_panel(msg: &str) -> bool {
    let m = msg.to_lowercase();
    PANEL_CUES.iter().any(|c| m.contains(c))
}

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
/// Parse a priority word (English or Vietnamese) into a domain [`Priority`].
fn parse_priority(s: &str) -> Option<coxagent_domain::ticket::Priority> {
    use coxagent_domain::ticket::Priority;
    match s.trim().to_lowercase().as_str() {
        "high" | "cao" | "urgent" | "khẩn" | "p0" | "p1" => Some(Priority::High),
        "medium" | "med" | "normal" | "trung bình" | "p2" => Some(Priority::Medium),
        "low" | "thấp" | "p3" => Some(Priority::Low),
        _ => None,
    }
}

fn strip_kw<'a>(a: &'a str, kw: &str) -> Option<&'a str> {
    let rest = a.strip_prefix(kw)?;
    Some(rest.trim_start_matches([':', ' ']).trim())
}

/// Split a trailing `ACTION: <directive>` line off the reply body.
/// Parse a `feature:`/`bug:` action payload: `title :: desc :: priority
/// [:: sprint]` — everything after the title optional. A trailing `sprint`
/// token means "commit the new ticket into the RUNNING sprint"; without it
/// the ticket waits in the backlog for rollover.
fn parse_ticket_payload(
    payload: &str,
) -> (String, String, Option<coxagent_domain::ticket::Priority>, bool) {
    let mut parts = payload.splitn(4, "::").map(str::trim);
    let title = parts.next().unwrap_or("").trim();
    let desc = parts.next().unwrap_or("");
    let mut prio_tok = parts.next().unwrap_or("");
    let mut tail = parts.next().unwrap_or("");
    if tail.is_empty() && prio_tok.eq_ignore_ascii_case("sprint") {
        // `title :: desc :: sprint` — priority omitted entirely.
        tail = prio_tok;
        prio_tok = "";
    }
    (
        title.to_owned(),
        desc.to_owned(),
        parse_priority(prio_tok),
        tail.eq_ignore_ascii_case("sprint"),
    )
}

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

#[cfg(test)]
mod panel_tests {
    use super::wants_panel;

    #[test]
    fn strategic_questions_get_the_panel_and_reports_do_not() {
        assert!(wants_panel("mình nên ưu tiên hướng nào cho quý sau?"));
        assert!(wants_panel("should we adopt a monorepo approach?"));
        assert!(wants_panel("cả team nghĩ sao về kế hoạch này"));
        assert!(!wants_panel("nút login bị lỗi 500"));
        assert!(!wants_panel("deploy lại đi"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_payload_parses_the_sprint_suffix_in_every_position() {
        use coxagent_domain::ticket::Priority;
        let (t, d, p, sp) = parse_ticket_payload("Paste images :: add listener :: high :: sprint");
        assert_eq!((t.as_str(), d.as_str(), p, sp), ("Paste images", "add listener", Some(Priority::High), true));
        let (_, _, p, sp) = parse_ticket_payload("Paste images :: add listener :: sprint");
        assert_eq!((p, sp), (None, true));
        let (_, _, p, sp) = parse_ticket_payload("Paste images :: add listener :: low");
        assert_eq!((p, sp), (Some(Priority::Low), false));
    }
    use crate::ports::outbound::{AgentOutcome, AgentRequest, DeployReport};
    use crate::state::ProjectState;
    use crate::PortError;
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
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    /// Never invoked by `deploy_now` — only needed to satisfy `E: AgentEnginePort`.
    struct UnusedEngine;
    #[async_trait::async_trait]
    impl AgentEnginePort for UnusedEngine {
        fn id(&self) -> &'static str {
            "unused"
        }
        async fn run(&self, _request: AgentRequest) -> Result<AgentOutcome, PortError> {
            unreachable!("deploy_now never calls the engine")
        }
    }

    /// `docker compose up` exits 0 (container started) but the app inside
    /// never answers on its configured port — `health()` reports down for
    /// every probe, same dead-on-arrival scenario as COX-B004.
    struct DeployWithDeadPort;
    #[async_trait::async_trait]
    impl DeployPort for DeployWithDeadPort {
        async fn deploy(&self, _work_dir: &std::path::Path) -> Result<DeployReport, PortError> {
            Ok(DeployReport {
                success: true,
                deployed: true,
                summary: "docker compose up -d --build succeeded".to_owned(),
            })
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Ok(false)
        }
    }

    struct HealthyDeploy;
    #[async_trait::async_trait]
    impl DeployPort for HealthyDeploy {
        async fn deploy(&self, _work_dir: &std::path::Path) -> Result<DeployReport, PortError> {
            Ok(DeployReport {
                success: true,
                deployed: true,
                summary: "docker compose up -d --build succeeded".to_owned(),
            })
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Ok(true)
        }
    }

    fn last_comment(store: &Arc<MemStore>) -> String {
        store
            .state
            .lock()
            .expect("lock")
            .comments
            .last()
            .expect("a comment was posted")
            .body
            .clone()
    }

    /// Already has a compose file, so `deploy_now` skips the scaffold path
    /// (which would otherwise call the engine — not needed by these tests).
    fn work_dir_with_compose() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("docker-compose.yml"), "services: {}").expect("write");
        dir
    }

    /// AC (COX-B009): the chat "deploy" command must run through the same
    /// mandatory health gate as the autonomous cycle (COX-B004) — a compose
    /// exit-0 that never binds the app's port must NOT be reported as "Deploy
    /// OK" to the human.
    #[tokio::test(start_paused = true)]
    async fn chat_deploy_reports_failure_when_the_app_never_binds_its_port() {
        let store = Arc::new(MemStore::default());
        let dir = work_dir_with_compose();
        let uc = RunChatReplyUseCase::new(
            Arc::clone(&store),
            Arc::new(UnusedEngine),
            dir.path().to_path_buf(),
            false,
            Language::En,
        )
        .with_deploy(Arc::new(DeployWithDeadPort) as Arc<dyn DeployPort>)
        .with_host_port_probe(Ok(Some(8101)));

        uc.deploy_now().await;

        let body = last_comment(&store);
        assert!(
            !body.contains("Deploy OK"),
            "a deploy that never binds its port must not be reported as OK: {body}"
        );
        assert!(
            body.contains("health check failed"),
            "expected the health-gate failure reason in the reply: {body}"
        );
    }

    /// Control: a deploy that actually answers on its port still reports OK —
    /// the gate must not fail a genuinely healthy deploy.
    #[tokio::test(start_paused = true)]
    async fn chat_deploy_reports_ok_when_the_app_is_healthy() {
        let store = Arc::new(MemStore::default());
        let dir = work_dir_with_compose();
        let uc = RunChatReplyUseCase::new(
            Arc::clone(&store),
            Arc::new(UnusedEngine),
            dir.path().to_path_buf(),
            false,
            Language::En,
        )
        .with_deploy(Arc::new(HealthyDeploy) as Arc<dyn DeployPort>)
        .with_host_port_probe(Ok(Some(8101)));

        uc.deploy_now().await;

        assert!(last_comment(&store).contains("Deploy OK"));
    }

    /// AC (COX-B035): a malformed `deploy.host_port` in the project's
    /// `coxagent.json` must fail the chat "deploy" command's health gate —
    /// not be folded into "nothing configured" (which would pass
    /// vacuously and report a possibly-dead deploy as OK), matching the
    /// PR-preview endpoint's COX-B025/COX-B026 fix. Uses a deploy adapter
    /// that would pass any real probe, so a false "Deploy OK" here would
    /// mean the malformed port silently skipped the gate.
    #[tokio::test(start_paused = true)]
    async fn chat_deploy_reports_failure_when_host_port_is_malformed() {
        let store = Arc::new(MemStore::default());
        let dir = work_dir_with_compose();
        let uc = RunChatReplyUseCase::new(
            Arc::clone(&store),
            Arc::new(UnusedEngine),
            dir.path().to_path_buf(),
            false,
            Language::En,
        )
        .with_deploy(Arc::new(HealthyDeploy) as Arc<dyn DeployPort>)
        .with_host_port_probe(Err(()));

        uc.deploy_now().await;

        let body = last_comment(&store);
        assert!(
            !body.contains("Deploy OK"),
            "a malformed host_port must not skip the health gate: {body}"
        );
    }
}
