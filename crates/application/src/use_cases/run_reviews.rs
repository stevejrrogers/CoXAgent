//! Periodic health reviews the team runs every few sprints — and can trigger on
//! demand: a whole-system **architecture** review (SA files refactor chores) and
//! a **documentation** review (DOCS fills Wiki gaps). Both are `?Sized` over the
//! ports so the hub can run them with `dyn` adapters, not just the cycle.

use crate::config::Language;
use crate::error::AppError;
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::ticket::{Status, TicketType};
use coxagent_domain::{Role, TicketId};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// SA whole-system architecture review → refactor chores + a PO nudge.
pub struct RunArchitectureAuditUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    token_saver: bool,
    lang: Language,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunArchitectureAuditUseCase<S, E> {
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

    /// Review the architecture and file refactor chores. Returns the count filed.
    ///
    /// # Errors
    /// [`AppError`] only on a store failure while saving; engine hiccups are
    /// swallowed (best-effort review).
    pub async fn execute(&self, sprint: u32) -> Result<usize, AppError> {
        let vi = self.lang.is_vi();
        let task = format!(
            "You are a Staff Solution Architect doing a WHOLE-SYSTEM architecture review of the \
             code in the working directory (use `.coxagent/REPO_MAP.md` to orient).{}\n\nAssess \
             honestly: clean/hexagonal layering & the dependency rule; DDD boundaries; SOLID and \
             coupling/cohesion; whether it should stay a modular monolith or split services (with \
             justification); and horizontal scalability (statelessness, shared/session state, the \
             data layer, caching). Identify concrete, high-value REFACTORS — not nitpicks.\n\n\
             Respond with ONLY a JSON array (empty [] if the architecture is genuinely solid), each \
             item exactly: {{\"title\": string, \"description\": string, \"priority\": \
             \"low\"|\"medium\"|\"high\", \"complexity\": \"small\"|\"medium\"|\"large\"}}. \
             `description` must name the files/modules and the target design.",
            crate::prompts::repo_map_block(&self.work_dir, self.token_saver)
        );
        let request = AgentRequest {
            role: Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(900),
        };
        let items = match self.engine.run(request).await {
            Ok(o) if o.succeeded() => crate::parsing::parse_items(&o.stdout).unwrap_or_default(),
            _ => Vec::new(),
        };
        if items.is_empty() {
            let msg = if vi {
                format!("🏛️ Rà soát kiến trúc (sau sprint {sprint}): kiến trúc đang ổn, chưa cần refactor lớn.")
            } else {
                format!("🏛️ Architecture review (after sprint {sprint}): the architecture is solid — no major refactor needed.")
            };
            self.post("SA", &msg).await;
            return Ok(0);
        }
        let adder = AddTicketUseCase::new(Arc::clone(&self.store));
        let mut filed: Vec<String> = Vec::new();
        for it in items.iter().take(8) {
            let marker = format!("Refactor: {}", it.title);
            if let Ok(state) = self.store.load().await {
                if state
                    .tickets
                    .iter()
                    .any(|t| t.title() == marker && t.status() != Status::Done)
                {
                    continue;
                }
            }
            if let Ok(id) = adder
                .execute(AddTicketInput {
                    ticket_type: TicketType::Chore,
                    title: marker,
                    description: format!(
                        "Architecture refactor (SA review, sprint {sprint}).\n\n{}",
                        it.description
                    ),
                    priority: it.priority,
                    complexity: it.complexity,
                    has_ui: false,
                    acceptance_criteria: Vec::new(),
                })
                .await
            {
                filed.push(id.to_string());
            }
        }
        let msg = if vi {
            format!(
                "🏛️ Rà soát kiến trúc (sau sprint {sprint}): SA đã tạo {} ticket refactor ({}). \
                 PO ơi, cân nhắc ưu tiên một sprint hardening để xử lý trước khi nợ kỹ thuật phình to.",
                filed.len(),
                filed.join(", ")
            )
        } else {
            format!(
                "🏛️ Architecture review (after sprint {sprint}): SA filed {} refactor ticket(s) ({}). \
                 PO, please consider prioritising a hardening sprint before the tech debt compounds.",
                filed.len(),
                filed.join(", ")
            )
        };
        if let Ok(mut s) = self.store.load().await {
            s.post_comment("SA", &msg, None);
            s.log_activity("SA", "architecture review", None);
            let _ = self.store.save(&s).await;
        }
        Ok(filed.len())
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut s) = self.store.load().await {
            s.post_comment(author, body, None);
            let _ = self.store.save(&s).await;
        }
    }
}

/// DOCS Wiki-gap review → writes missing pages in full.
pub struct RunDocsAuditUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    lang: Language,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunDocsAuditUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf, lang: Language) -> Self {
        Self {
            store,
            engine,
            work_dir,
            lang,
        }
    }

    /// Fill Wiki gaps for shipped work. Returns the number of pages written.
    ///
    /// # Errors
    /// [`AppError`] only on a store failure while saving.
    pub async fn execute(&self, sprint: u32) -> Result<usize, AppError> {
        let vi = self.lang.is_vi();
        let Ok(state) = self.store.load().await else {
            return Ok(0);
        };
        let mut targets: Vec<(TicketId, String, TicketType)> = Vec::new();
        for t in &state.tickets {
            if !matches!(
                t.status(),
                Status::Done | Status::Documented | Status::Verified
            ) {
                continue;
            }
            let doc_id = format!("feat-{}", t.id());
            let thin = state
                .docs
                .iter()
                .find(|d| d.id == doc_id)
                .map_or(true, |d| d.body.trim().len() < 200);
            if thin {
                targets.push((t.id().clone(), t.title().to_owned(), t.ticket_type()));
            }
        }
        if targets.is_empty() {
            let msg = if vi {
                format!("📚 Rà soát tài liệu (sau sprint {sprint}): Wiki đã đầy đủ, không thiếu trang nào.")
            } else {
                format!("📚 Docs review (after sprint {sprint}): the Wiki is complete — nothing missing.")
            };
            self.post("DOCS", &msg).await;
            return Ok(0);
        }
        let mut written: Vec<String> = Vec::new();
        for (id, title, ttype) in targets.iter().take(5) {
            let request = AgentRequest {
                role: Role::Docs,
                system_prompt: crate::prompts::system_prompt(crate::prompts::DOCS),
                task_prompt: format!("Document ticket {id}: {title}"),
                work_dir: self.work_dir.clone(),
                timeout: Duration::from_secs(900),
            };
            let body = match self.engine.run(request).await {
                Ok(o) if o.succeeded() => o.stdout.trim().to_owned(),
                _ => continue,
            };
            if body.len() < 200 {
                continue;
            }
            let folder = crate::state::standard_doc_folder(*ttype);
            let category = crate::state::doc_category_of(folder);
            if let Ok(mut s) = self.store.load().await {
                s.ensure_standard_folders();
                s.upsert_doc(
                    &format!("feat-{id}"),
                    folder,
                    category,
                    title,
                    &body,
                    "DOCS",
                );
                let _ = self.store.save(&s).await;
                written.push(id.to_string());
            }
        }
        let msg = if vi {
            format!(
                "📚 Rà soát tài liệu (sau sprint {sprint}): DOCS đã viết {} trang còn thiếu ({}).",
                written.len(),
                written.join(", ")
            )
        } else {
            format!(
                "📚 Docs review (after sprint {sprint}): DOCS wrote {} missing page(s) ({}).",
                written.len(),
                written.join(", ")
            )
        };
        if let Ok(mut s) = self.store.load().await {
            s.post_comment("DOCS", &msg, None);
            s.log_activity("DOCS", "documentation review", None);
            let _ = self.store.save(&s).await;
        }
        Ok(written.len())
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut s) = self.store.load().await {
            s.post_comment(author, body, None);
            let _ = self.store.save(&s).await;
        }
    }
}
