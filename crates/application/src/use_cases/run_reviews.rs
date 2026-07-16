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
        let evidence = gather_evidence(&self.work_dir);
        let task = format!(
            "You are a Staff Solution Architect doing a WHOLE-SYSTEM architecture review. You have \
             FULL read access to the working directory — actually OPEN and READ the code, don't \
             guess. Investigate and ground EVERY statement in specific files you found:\n\
             - Architecture & clean/hexagonal layering: are there domain / application / adapter \
             layers, and does the dependency rule hold (edges → core, never the reverse)? Name the \
             modules; say if it's actually layered or a big ball of mud.\n\
             - Persistence: which datastore(s) are ACTUALLY used (Postgres/Mongo/SQLite/plain \
             files/none), the driver/ORM, and where the repository/adapter lives. If there's no \
             real DB, say so.\n\
             - Caching: is there any (in-memory / Redis / HTTP)? where — and what's missing.\n\
             - Concurrency & consistency: async model, locks, transactions, atomic writes, race \
             handling.\n\
             - Horizontal scalability: stateless or not, shared/session/in-proc state, whether it \
             can run behind N instances.\n\nEvidence to start from (read further as needed):\n\
             {evidence}\n\nRespond with ONLY a JSON object, no prose:\n\
             {{\"assessment\": {{\"architecture\": string, \"persistence\": string, \"caching\": \
             string, \"concurrency\": string, \"scalability\": string, \"verdict\": string}}, \
             \"refactors\": [{{\"title\": string, \"description\": string, \"priority\": \
             \"low\"|\"medium\"|\"high\", \"complexity\": \"small\"|\"medium\"|\"large\"}}]}}\n\
             Each assessment field: 1-3 concrete sentences citing real files/modules. `refactors` \
             lists concrete high-value work (name the files + target design); [] only if genuinely \
             solid.{}{}",
            self.lang.reply_directive(),
            crate::prompts::repo_map_block(&self.work_dir, self.token_saver)
        );
        let request = AgentRequest {
            role: Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1200),
        };
        let raw = match self.engine.run(request).await {
            Ok(o) if o.succeeded() => o.stdout,
            _ => return Ok(0),
        };
        let obj = parse_object(&raw);
        let items: Vec<crate::parsing::ProposedItem> = obj
            .get("refactors")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        // Post the written assessment (and save it as a Wiki page) so the review
        // is a real report, not just tickets.
        let report = render_assessment(obj.get("assessment"), sprint, vi);
        if !report.is_empty() {
            if let Ok(mut s) = self.store.load().await {
                s.ensure_standard_folders();
                s.post_comment("SA", &report, None);
                s.upsert_doc(
                    "arch-review",
                    "Architecture",
                    crate::state::doc_category_of("Architecture"),
                    "Architecture Review",
                    &report,
                    "SA",
                );
                let _ = self.store.save(&s).await;
            }
        }

        if items.is_empty() {
            let msg = if vi {
                format!("🏛️ Rà soát kiến trúc (sau sprint {sprint}): kiến trúc đang ổn, chưa cần refactor lớn.")
            } else {
                format!("🏛️ Architecture review (after sprint {sprint}): the architecture is solid — no major refactor needed.")
            };
            self.post("SA", &msg).await;
            return Ok(0);
        }
        self.file_refactors(&items, sprint, vi).await
    }

    /// File the SA's refactor recommendations as deduped chores and nudge the PO.
    async fn file_refactors(
        &self,
        items: &[crate::parsing::ProposedItem],
        sprint: u32,
        vi: bool,
    ) -> Result<usize, AppError> {
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

/// Extract the outermost JSON object from engine output (tolerating prose).
fn parse_object(raw: &str) -> serde_json::Value {
    let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) else {
        return serde_json::Value::Null;
    };
    if end <= start {
        return serde_json::Value::Null;
    }
    serde_json::from_str(&raw[start..=end]).unwrap_or(serde_json::Value::Null)
}

/// Render the assessment object into a readable Markdown report.
fn render_assessment(assessment: Option<&serde_json::Value>, sprint: u32, vi: bool) -> String {
    use std::fmt::Write as _;
    let Some(a) = assessment.filter(|v| v.is_object()) else {
        return String::new();
    };
    let get = |k: &str| {
        a.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
    };
    let fields: [(&str, &str, &str); 6] = [
        (
            "architecture",
            "Kiến trúc & phân tầng",
            "Architecture & layering",
        ),
        ("persistence", "Lưu trữ / DB", "Persistence / DB"),
        ("caching", "Cache", "Caching"),
        (
            "concurrency",
            "Concurrency & nhất quán",
            "Concurrency & consistency",
        ),
        (
            "scalability",
            "Khả năng scale ngang",
            "Horizontal scalability",
        ),
        ("verdict", "Kết luận", "Verdict"),
    ];
    let mut out = if vi {
        format!("🏛️ **Rà soát kiến trúc — sau sprint {sprint}**\n")
    } else {
        format!("🏛️ **Architecture review — after sprint {sprint}**\n")
    };
    let mut any = false;
    for (key, vi_label, en_label) in fields {
        let v = get(key);
        if v.is_empty() {
            continue;
        }
        any = true;
        let label = if vi { vi_label } else { en_label };
        let _ = write!(out, "\n**{label}:** {v}");
    }
    if any {
        out
    } else {
        String::new()
    }
}

/// Collect real architecture evidence for the SA: dependency manifests (which
/// reveal the DB/cache/framework) and the top folder layout (which reveals the
/// layering). Bounded so it never blows the prompt.
fn gather_evidence(work_dir: &std::path::Path) -> String {
    use std::fmt::Write as _;
    const MANIFESTS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "go.mod",
        "requirements.txt",
        "pyproject.toml",
        "pom.xml",
        "build.gradle",
        "Gemfile",
        "composer.json",
        "docker-compose.yml",
        "compose.yml",
    ];
    let ignore = |n: &str| n.starts_with('.') || n == "target" || n == "node_modules";

    // Manifest files at the root and one/two levels down (workspace members).
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for m in MANIFESTS {
        let p = work_dir.join(m);
        if p.exists() {
            files.push(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(work_dir) {
        for e in rd.flatten().filter(|e| e.path().is_dir()) {
            if e.file_name().to_str().is_some_and(ignore) {
                continue;
            }
            for m in ["Cargo.toml", "package.json", "go.mod"] {
                let p = e.path().join(m);
                if p.exists() {
                    files.push(p);
                }
            }
            if let Ok(rd2) = std::fs::read_dir(e.path()) {
                for e2 in rd2.flatten().filter(|e| e.path().is_dir()) {
                    for m in ["Cargo.toml", "package.json"] {
                        let p = e2.path().join(m);
                        if p.exists() {
                            files.push(p);
                        }
                    }
                }
            }
        }
    }

    let mut out = String::from("## Dependency manifests\n");
    let mut budget: usize = 6000;
    for f in files.iter().take(24) {
        if budget == 0 {
            break;
        }
        if let Ok(text) = std::fs::read_to_string(f) {
            let rel = f.strip_prefix(work_dir).unwrap_or(f);
            let snippet: String = text.chars().take(budget.min(1200)).collect();
            budget = budget.saturating_sub(snippet.len());
            let _ = writeln!(out, "\n### {}\n```\n{snippet}\n```", rel.display());
        }
    }

    out.push_str("\n## Directory layout (folders, depth 2)\n");
    if let Ok(rd) = std::fs::read_dir(work_dir) {
        let mut dirs: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !ignore(n))
            .collect();
        dirs.sort();
        for d in dirs.iter().take(40) {
            let _ = writeln!(out, "- {d}/");
            if let Ok(rd2) = std::fs::read_dir(work_dir.join(d)) {
                let mut subs: Vec<String> = rd2
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| !ignore(n))
                    .collect();
                subs.sort();
                for s in subs.iter().take(20) {
                    let _ = writeln!(out, "  - {s}/");
                }
            }
        }
    }
    out
}
