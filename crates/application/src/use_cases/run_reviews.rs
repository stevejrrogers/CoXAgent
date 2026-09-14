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
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
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
            files: None,
        }
    }

    /// Attach the files port so the evidence pass can scan manifests and the
    /// directory layout. Without it the review runs on prompt knowledge alone.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    /// Review the architecture and file refactor chores. Returns the count filed.
    ///
    /// # Errors
    /// [`AppError`] only on a store failure while saving; engine hiccups are
    /// swallowed (best-effort review).
    pub async fn execute(&self, sprint: u32) -> Result<usize, AppError> {
        let vi = self.lang.is_vi();
        let evidence = match &self.files {
            Some(files) => gather_evidence(files.as_ref(), &self.work_dir).await,
            None => String::new(),
        };
        let task = format!(
            "You are a Staff Solution Architect doing a WHOLE-SYSTEM architecture review. You have \
             FULL read access to the working directory — actually OPEN and READ the code, don't \
             guess. Investigate and ground EVERY statement in specific files you found:\n\
             - Architecture & clean/hexagonal layering: domain/application/adapter layers, and the \
             dependency rule (edges → core). Layered, or a big ball of mud?\n\
             - Persistence: which datastore(s) are ACTUALLY used, the driver/ORM, schema/migrations, \
             indexing, and where the repository/adapter lives. If there's no real DB, say so.\n\
             - Caching: any (in-memory / Redis / HTTP)? where — and what's missing.\n\
             - Concurrency & consistency: async model, locks, transactions, atomic writes, races.\n\
             - Scalability & performance: stateless or not, shared/in-proc state, N-instance \
             readiness, obvious bottlenecks (N+1, unbounded work).\n\
             - Reliability & resilience: failure modes, retries/timeouts/idempotency, health checks, \
             backups/DR, graceful degradation.\n\
             - Security: authN/authZ, secrets handling, encryption in transit/at rest, input \
             validation, OWASP issues, vulnerable deps, audit logging.\n\
             - Observability: logging, metrics, tracing, alerting — enough to operate it?\n\
             - API & integration: contract clarity, versioning/backward-compat, error handling, \
             coupling to third parties / lock-in.\n\
             - Testability & quality: test strategy & coverage of critical paths, CI/CD gates.\n\
             - Cost & tech choices: stack fit, build-vs-buy, licensing/cloud cost risks.\n\n\
             Then judge the LONG-TERM risk: if the foundation is weak, building more features on it \
             just compounds the mess — in that case recommend HALTING new features for a hardening \
             sprint.\n\nEvidence to start from (read further as needed):\n{evidence}\n\n\
             Respond with ONLY a JSON object, no prose:\n{{\"assessment\": {{\"architecture\": \
             string, \"persistence\": string, \"caching\": string, \"concurrency\": string, \
             \"scalability\": string, \"reliability\": string, \"security\": string, \
             \"observability\": string, \"api\": string, \"testability\": string, \"cost\": string, \
             \"verdict\": string}}, \"risk\": \"low\"|\"medium\"|\"high\"|\"critical\", \
             \"halt_for_refactor\": boolean, \"refactors\": [{{\"title\": string, \"description\": \
             string, \"priority\": \"low\"|\"medium\"|\"high\", \"complexity\": \
             \"small\"|\"medium\"|\"large\"}}]}}\n\
             Each assessment field: 1-3 concrete sentences citing real files/modules (write \
             \"n/a\" if truly not applicable). Set halt_for_refactor=true only when the risk is \
             high/critical and continuing to add features would make it worse. `refactors` names \
             concrete work (files + target design); [] only if genuinely solid.{}{}",
            self.lang.reply_directive(),
            crate::prompts::repo_map_block(self.files.as_deref(), &self.work_dir, self.token_saver)
                .await
        );
        let request = AgentRequest {
            role: Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1200),
            escalation_level: 0,
            label: None,
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
        let risk = obj
            .get("risk")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let halt = obj
            .get("halt_for_refactor")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || risk == "high"
            || risk == "critical";

        // Post the written assessment (and save it as a Wiki page) so the review
        // is a real report, not just tickets.
        self.post_assessment(render_assessment(obj.get("assessment"), &risk, sprint, vi))
            .await;
        self.bank_verdict(&obj).await;

        if items.is_empty() {
            let msg = if vi {
                format!("🏛️ Rà soát kiến trúc (sau sprint {sprint}): kiến trúc đang ổn, chưa cần refactor lớn.")
            } else {
                format!("🏛️ Architecture review (after sprint {sprint}): the architecture is solid — no major refactor needed.")
            };
            self.post("SA", &msg).await;
            return Ok(0);
        }
        let filed = self.file_refactors(&items, sprint, vi).await?;
        if halt && filed > 0 {
            self.call_refactor_sprint(vi).await;
        }
        Ok(filed)
    }

    /// Bank the architecture verdict as a durable team decision so every agent
    /// honours it later.
    async fn bank_verdict(&self, obj: &serde_json::Value) {
        let Some(v) = obj
            .get("assessment")
            .and_then(|a| a.get("verdict"))
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.trim().is_empty())
        else {
            return;
        };
        if let Ok(mut s) = self.store.load().await {
            s.add_decision(&format!("Architecture: {}", v.trim()));
            let _ = self.store.save(&s).await;
        }
    }

    /// Post the assessment to the feed and save it as the Architecture Review
    /// Wiki page (no-op when empty).
    async fn post_assessment(&self, report: String) {
        if report.is_empty() {
            return;
        }
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

    /// The SA calls a halt: flag a refactor sprint so BA stops proposing features
    /// and planning dedicates the next sprint to the refactor chores.
    async fn call_refactor_sprint(&self, vi: bool) {
        if let Ok(mut s) = self.store.load().await {
            if s.refactor_mode {
                return;
            }
            s.refactor_mode = true;
            let msg = if vi {
                "🛑 SA yêu cầu DỪNG thêm feature: nền tảng đang rủi ro cao, càng xây thêm càng tệ. \
                 Sprint tới là REFACTOR SPRINT — ưu tiên dọn các ticket refactor, SA sẽ giám sát \
                 chất lượng và cập nhật lại technical spec cho các feature dính kiến trúc cũ."
            } else {
                "🛑 SA is calling a HALT on new features: the foundation is high-risk and building \
                 more only makes it worse. Next sprint is a REFACTOR SPRINT — the refactor chores \
                 come first, and the SA will supervise quality and realign feature specs affected \
                 by the old architecture."
            };
            s.post_comment("SA", msg, None);
            s.log_activity("SA", "called a refactor sprint", None);
            let _ = self.store.save(&s).await;
        }
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
                    goal: None,
                    service_tag: None,
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
                // One prompt for both routes into the wiki: this path used to
                // ask for "documentation" with no shape at all, and got 300
                // characters back.
                task_prompt: crate::use_cases::run_docs::build_docs_prompt(
                    id,
                    title,
                    crate::state::standard_doc_folder(*ttype),
                    &[],
                    None,
                ),
                work_dir: self.work_dir.clone(),
                timeout: Duration::from_secs(900),
                escalation_level: 0,
                label: None,
            };
            let body = match self.engine.run(request).await {
                Ok(o) if o.succeeded() => o.stdout.trim().to_owned(),
                _ => continue,
            };
            // The same structure gate the per-ticket writer enforces. Without
            // it this path filled the wiki with 300-character stubs hours after
            // the gate shipped — pages the next agent cannot navigate and the
            // refresher then has to rewrite one per idle cycle.
            if let Some(missing) =
                crate::use_cases::run_docs::docs_gate_failures(&body, &self.work_dir)
            {
                tracing::warn!(
                    "docs review: page for {id} rejected by the structure gate: {missing}"
                );
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
fn render_assessment(
    assessment: Option<&serde_json::Value>,
    risk: &str,
    sprint: u32,
    vi: bool,
) -> String {
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
    let fields: [(&str, &str, &str); 12] = [
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
            "Scale & hiệu năng",
            "Scalability & performance",
        ),
        (
            "reliability",
            "Độ tin cậy & resilience",
            "Reliability & resilience",
        ),
        ("security", "Bảo mật", "Security"),
        ("observability", "Observability", "Observability"),
        ("api", "API & tích hợp", "API & integration"),
        (
            "testability",
            "Kiểm thử & chất lượng",
            "Testability & quality",
        ),
        ("cost", "Chi phí & công nghệ", "Cost & tech choices"),
        ("verdict", "Kết luận", "Verdict"),
    ];
    let mut out = if vi {
        format!("🏛️ **Rà soát kiến trúc — sau sprint {sprint}**")
    } else {
        format!("🏛️ **Architecture review — after sprint {sprint}**")
    };
    if !risk.is_empty() {
        let label = if vi { "Rủi ro" } else { "Risk" };
        let _ = write!(out, " · **{label}: {}**", risk.to_uppercase());
    }
    out.push('\n');
    let mut any = false;
    for (key, vi_label, en_label) in fields {
        let v = get(key);
        if v.is_empty() || v.eq_ignore_ascii_case("n/a") {
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

/// Gather grounding evidence for the review: dependency manifests (which
/// reveal the DB/cache/framework) and the top folder layout (which reveals the
/// layering). Bounded so it never blows the prompt. All reads go through the
/// files port — the ratchet forbids `std::fs` here.
async fn gather_evidence(
    files: &dyn crate::ports::outbound::WorkspaceFilesPort,
    work_dir: &std::path::Path,
) -> String {
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
    let ignore = |p: &std::path::Path| {
        p.file_name().and_then(|n| n.to_str()).map_or(true, |n| {
            n.starts_with('.') || n == "target" || n == "node_modules"
        })
    };

    // Manifest files at the root and one/two levels down (workspace members),
    // read as we find them so one port call per candidate suffices.
    let mut found: Vec<(std::path::PathBuf, String)> = Vec::new();
    for m in MANIFESTS {
        if let Some(text) = files.read(&work_dir.join(m)).await {
            found.push((work_dir.join(m), text));
        }
    }
    let top_dirs: Vec<std::path::PathBuf> = files
        .list_dirs(work_dir)
        .await
        .into_iter()
        .filter(|d| !ignore(d))
        .collect();
    for d in &top_dirs {
        for m in ["Cargo.toml", "package.json", "go.mod"] {
            if let Some(text) = files.read(&d.join(m)).await {
                found.push((d.join(m), text));
            }
        }
        for d2 in files.list_dirs(d).await.into_iter().filter(|d| !ignore(d)) {
            for m in ["Cargo.toml", "package.json"] {
                if let Some(text) = files.read(&d2.join(m)).await {
                    found.push((d2.join(m), text));
                }
            }
        }
    }

    let mut out = String::from("## Dependency manifests\n");
    let mut budget: usize = 6000;
    for (f, text) in found.iter().take(24) {
        if budget == 0 {
            break;
        }
        let rel = f.strip_prefix(work_dir).unwrap_or(f);
        let snippet: String = text.chars().take(budget.min(1200)).collect();
        budget = budget.saturating_sub(snippet.len());
        let _ = writeln!(out, "\n### {}\n```\n{snippet}\n```", rel.display());
    }

    out.push_str("\n## Directory layout (folders, depth 2)\n");
    for d in top_dirs.iter().take(40) {
        let name = d.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let _ = writeln!(out, "- {name}/");
        for sub in files
            .list_dirs(d)
            .await
            .into_iter()
            .filter(|d| !ignore(d))
            .take(20)
        {
            let sub = sub.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let _ = writeln!(out, "  - {sub}/");
        }
    }
    out
}
