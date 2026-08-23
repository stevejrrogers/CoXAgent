//! `RunPdUseCase` — the PD (Product Designer) design gate. Picks the top
//! pending UI feature that has a technical design but no UX yet, runs the engine
//! to author the UX design, attaches it, and moves the ticket to `ready`. The
//! aggregate re-checks Definition of Ready, so a UI ticket only advances once
//! both technical and UX designs exist — enforced by code, not the prompt.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::selection::ux_candidates;
use coxagent_domain::{Role, Status, TicketId, UxDesign};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The PD's JSON output: a UX design for one UI feature.
#[derive(Debug, Deserialize)]
struct UxOutput {
    #[serde(default)]
    user_flow: String,
    #[serde(default)]
    screens: Vec<String>,
    #[serde(default)]
    component_states: Vec<String>,
    #[serde(default)]
    responsive_notes: String,
}

/// Runs one PD UX-design pass.
pub struct RunPdUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    worker: String,
    phase: Option<crate::use_cases::runner::PhaseReporter>,
    context: Option<String>,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    storage: Option<Arc<dyn crate::ports::outbound::StoragePort>>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunPdUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            worker: String::new(),
            phase: None,
            context: None,
            files: None,
            storage: None,
        }
    }

    /// Attach blob storage (MinIO/S3 or the local blob dir) so PD design
    /// images dropped in `.coxagent/design/<ticket>/` are ingested onto the
    /// ticket; `None` (tests, unwired runners) skips ingestion.
    #[must_use]
    pub fn with_storage(
        mut self,
        storage: Option<Arc<dyn crate::ports::outbound::StoragePort>>,
    ) -> Self {
        self.storage = storage;
        self
    }

    /// Attach workspace file access for prompt context blocks; `None` (tests)
    /// reads as no context.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    #[must_use]
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// Set this runner's identity (`account@host`) so the PD stage is claimed
    /// per-ticket for parallel-safe UX design across concurrent runners.
    #[must_use]
    pub fn with_worker(mut self, worker: impl Into<String>) -> Self {
        self.worker = worker.into();
        self
    }

    /// Attach the live "working now" reporter; fired only after the stage is won.
    #[must_use]
    pub fn with_phase(mut self, phase: Option<crate::use_cases::runner::PhaseReporter>) -> Self {
        self.phase = phase;
        self
    }

    /// The UX design in the engine's stdout, giving a malformed answer one
    /// repair pass before giving up.
    ///
    /// `Err` carries the ORIGINAL parse error, not the repair's: what the model
    /// first got wrong is the useful thing to read, while a failed repair only
    /// says the second attempt was also unparseable.
    async fn parse_or_repair_ux(&self, stdout: &str) -> Result<UxOutput, String> {
        match parse_ux(stdout) {
            Ok(u) => Ok(u),
            Err(first) => {
                let fixed = crate::use_cases::repair_json(
                    self.engine.as_ref(),
                    stdout,
                    "a JSON object with the UX design fields",
                    &self.work_dir,
                )
                .await;
                match fixed.as_deref().map(parse_ux) {
                    Some(Ok(repaired)) => Ok(repaired),
                    _ => Err(first),
                }
            }
        }
    }

    /// Author UX for the next pending UI feature awaiting it. Returns the
    /// readied ticket id, or `None` when nothing needs UX.
    ///
    /// # Errors
    /// [`AppError`] on engine failure, unparseable output, or a DoR violation.
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        let state = self.store.load().await?;
        let worker = if self.worker.is_empty() {
            "local".to_owned()
        } else {
            self.worker.clone()
        };
        let now = crate::state::now_rfc3339();
        let mut chosen = None;
        for cand in ux_candidates(&state) {
            if self.store.claim_stage(&cand, "pd", &worker, &now).await? {
                chosen = Some(cand);
                break;
            }
        }
        let Some(id) = chosen else {
            return Ok(None);
        };
        if let Some(p) = &self.phase {
            p(Some(("PD".to_owned(), id.to_string())));
        }
        let title = state
            .ticket(&id)
            .map_or("", coxagent_domain::Ticket::title)
            .to_owned();

        let memory = prompts::team_memory_block(&state.decisions, &state.lessons);
        let steering = prompts::human_steering_block(&state, id.as_str());
        // The product's existing look and its earlier UX decisions live in the
        // team's own pages; designing without them is how a second design
        // language gets born.
        let knowledge = prompts::knowledge_block(
            self.files.as_deref(),
            &state.docs,
            &state.tickets,
            &self.work_dir,
            &format!(
                "{title} {}",
                state
                    .ticket(&id)
                    .map_or("", coxagent_domain::Ticket::description)
            ),
            &id.to_string(),
        )
        .await;
        let outcome = self
            .engine
            .run(
                self.build_request(&id, &title, &memory, &knowledge, &steering)
                    .await,
            )
            .await?;
        if !outcome.succeeded() {
            self.store.release_stage(&id, "pd", &worker).await.ok();
            return Err(PortError::Backend(format!(
                "PD engine failed on {id}: {}",
                outcome.failure_detail()
            ))
            .into());
        }
        let ux = match self.parse_or_repair_ux(&outcome.stdout).await {
            Ok(u) => u,
            Err(first) => {
                self.store.release_stage(&id, "pd", &worker).await.ok();
                return Err(PortError::Corrupt(format!("PD output: {first}")).into());
            }
        };

        // Atomic read-modify-write with retry (parallel-safe).
        let ux_design = ux_of(&ux);
        let gate_ready = self.config.workflow.human.gate_ready;
        crate::ports::outbound::mutate_state(self.store.as_ref(), |state| {
            let ticket = state
                .ticket_mut(&id)
                .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished")))?;
            ticket
                .set_ux_design(Role::Pd, ux_design.clone())
                .map_err(|e| PortError::Corrupt(e.to_string()))?;
            // DoR re-checked here; passes now that both technical and UX exist.
            // With the human ready-gate on, the designed ticket waits in
            // Pending for a person's approval instead.
            if gate_ready {
                let msg = format!(
                    "🧑‍⚖️ {id} is fully designed and WAITS for a human approval to Ready — \
                     it is in the Inbox (workflow.human.gate_ready)."
                );
                state.post_chat_in("SYSTEM", &msg, crate::state::APPROVALS_CHANNEL, Vec::new());
            } else {
                ticket
                    .transition_to(Role::Pd, Status::Ready)
                    .map_err(|e| PortError::Corrupt(e.to_string()))?;
            }
            Ok(())
        })
        .await?;
        // Ingest any design images PD dropped in `.coxagent/design/<id>/` —
        // the visuals travel with the ticket (MinIO/S3 or the local blob dir),
        // so a person can SEE the proposed design instead of reading it.
        self.attach_design_images(&id).await;
        if let Some(p) = &self.phase {
            p(None);
        }
        Ok(Some(id))
    }

    /// Ingest PD's dropped design files and record them on the ticket (with a
    /// comment), so the visuals show up beside the spec.
    async fn attach_design_images(&self, id: &TicketId) {
        let (recs, rejected) = self.ingest_design_files(id).await;
        if recs.is_empty() && rejected.is_empty() {
            return;
        }
        let n = recs.len();
        crate::ports::outbound::mutate_state(self.store.as_ref(), |state| {
            if !recs.is_empty() {
                state
                    .ticket_attachments
                    .entry(id.to_string())
                    .or_default()
                    .extend(recs.iter().cloned());
            }
            if !rejected.is_empty() {
                state.post_comment(
                    "PD",
                    &format!(
                        "🚫 skipped {} malformed/blank SVG(s) not attached to {id}: {}",
                        rejected.len(),
                        rejected.join(", ")
                    ),
                    Some(id.to_string()),
                );
            }
            if n > 0 {
                state.post_comment(
                    "PD",
                    &format!("🎨 attached {n} design image(s) to {id}."),
                    Some(id.to_string()),
                );
            }
            Ok(())
        })
        .await
        .ok();
    }

    /// Sweep `.coxagent/design/<ticket>/` for SVG mockups PD wrote, push each
    /// into blob storage and CONSUME the file (so a re-run never re-attaches).
    /// Pure orchestration: all I/O goes through the files + storage ports.
    async fn ingest_design_files(
        &self,
        id: &TicketId,
    ) -> (Vec<crate::state::TicketAttachment>, Vec<String>) {
        let (Some(files), Some(storage)) = (self.files.as_deref(), self.storage.as_deref()) else {
            return (Vec::new(), Vec::new());
        };
        let dir = self
            .work_dir
            .join(".coxagent")
            .join("design")
            .join(id.as_str());
        let mut out = Vec::new();
        let mut rejected = Vec::new();
        for meta in files.list(&dir).await {
            let name = meta
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // SVG only: the files port reads text, and SVG is what the PD
            // prompt asks for. Path-safe key: both parts are sanitised.
            if !name.to_ascii_lowercase().ends_with(".svg") {
                continue;
            }
            let path = dir.join(&name);
            let Some(body) = files.read(&path).await else {
                continue;
            };
            // Red gate for design artifacts: a mockup that is not renderable
            // (malformed XML or blank) is a broken deliverable — reject it
            // before attaching rather than showing the human a broken image.
            if !renderable_svg(&body) {
                tracing::warn!("rejecting malformed/blank SVG design file {name}");
                rejected.push(name);
                continue;
            }
            let safe = |s: &str| -> String {
                s.chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect()
            };
            let key = format!("design/{}/{}", safe(id.as_str()), safe(&name));
            match storage.put(&key, body.as_bytes(), "image/svg+xml").await {
                Ok(()) => {
                    files.delete(&path).await;
                    out.push(crate::state::TicketAttachment {
                        name,
                        key,
                        content_type: "image/svg+xml".to_owned(),
                        by: "PD".to_owned(),
                        at: crate::state::now_rfc3339(),
                    });
                }
                Err(e) => tracing::warn!("could not store design file {name}: {e}"),
            }
        }
        (out, rejected)
    }

    /// Pure renderability gate for a downloaded SVG mockup.
    async fn build_request(
        &self,
        id: &TicketId,
        title: &str,
        memory: &str,
        knowledge: &str,
        steering: &str,
    ) -> AgentRequest {
        let _choice = self.config.engine.resolve(Role::Pd);
        let context_block = self
            .context
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(|c| format!("\n\n## Project context (goal, stack, scope, design system — stay consistent):\n{c}\n"))
            .unwrap_or_default();
        AgentRequest {
            role: Role::Pd,
            system_prompt: prompts::system_prompt(prompts::PD),
            task_prompt: format!(
                "Design the UX for feature {id}: {title}\n\
                 Also SAVE visual mockups of the key screens as standalone SVG files \
                 under `.coxagent/design/{id}/` (one file per screen, e.g. \
                 `login.svg` — real layout, labels and states, not a placeholder \
                 box). They are attached to the ticket for the humans to review, \
                 so each SVG must be well-formed, renderable XML: quote every \
                 attribute (`x=\"40\"`), use only XML entities (no `&middot;`/`&nbsp;`), \
                 and contain real visible content — never an empty `<svg></svg>`. \
                 Validate the file before finishing.\
                 {context_block}{knowledge}{memory}{steering}{}{}",
                prompts::focus_block(self.files.as_deref(), &self.work_dir, title).await,
                prompts::repo_map_block(
                    self.files.as_deref(),
                    &self.work_dir,
                    self.config.workflow.token_saver,
                )
                .await,
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1200),
            escalation_level: 0,
            label: Some(id.to_string()),
        }
    }
}

/// Pure renderability gate for a downloaded SVG mockup. Detects the three
/// concrete corruption classes the PD agent produces (HTML entities valid only
/// in non-XML HTML, unquoted attribute values, and empty `<svg></svg>` stubs) so
/// broken mockups never reach the human's view. No XML parser is a dependency,
/// so this is a lightweight, self-contained well-formedness check that is
/// deliberately conservative about the real corruption patterns rather than
/// attempting full XML validation.
fn renderable_svg(body: &str) -> bool {
    let trimmed = body.trim_start();
    if !trimmed.starts_with("<svg") {
        return false;
    }
    if !body.trim_end().ends_with("</svg>") {
        return false;
    }
    if regex_lite_like_named_entity(body) {
        return false;
    }
    if has_unquoted_attribute(body) {
        return false;
    }
    // Blank stub: body is <svg ...> </svg> with no real nested element. Look
    // for any '<' between the opening tag's '>' and the closing </svg>; its
    // ABSENCE means the SVG renders as an empty box, which is useless.
    let rest = &body[..body.rfind("</svg>").unwrap_or(body.len())];
    let open_end = rest.find('>').map_or(rest.len(), |i| i + 1);
    let between = &rest[open_end..];
    if !between.contains('<') {
        return false;
    }
    true
}

/// Reject named entities (`&middot;`, `&nbsp;`, ...) that are invalid in XML,
/// while allowing the five XML entities and numeric char refs (`&#183;`,
/// `&#xA0;`). SVG in the SAVE files is XML, so HTML-only entities break it.
fn regex_lite_like_named_entity(s: &str) -> bool {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 1 < n {
        if bytes[i] == b'&' {
            // Find the closing ';' within a short lookahead.
            let end = (i + 2..n).take(12).find(|&j| bytes[j] == b';');
            if let Some(end) = end {
                let name = &s[i + 1..end];
                if !name.starts_with('#')
                    && !matches!(name, "amp" | "lt" | "gt" | "quot" | "apos")
                {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// Detect an unquoted attribute value (`<text x=40 y=52>`) by looking for `=`
/// not followed by a quote, space, `>`, or `/` inside a tag. Legit SVG uses
/// `x="40"`, so an unquoted one is the second corruption class.
fn has_unquoted_attribute(s: &str) -> bool {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'=' {
            if let Some(hit) = scan_unquoted_attr(&s[i..]) {
                if hit {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// Scan the run of characters immediately after a '='. Returns None if the
/// '=' is immediately followed by a quote (quoted value), or Some(false) when
/// it is structurally safe (end of input, whitespace, '>' or '/'). Returns
/// Some(true) when the next non-'/' char is a bare value start — unquoted.
fn scan_unquoted_attr(rest: &str) -> Option<bool> {
    let b = rest.as_bytes();
    if b.len() < 2 {
        return None;
    }
    match b[1] {
        b'"' | b'\'' => None,
        b'>' | b'/' | b' ' | b'\t' | b'\n' | b'\r' => Some(false),
        _ => Some(true),
    }
}

fn parse_ux(raw: &str) -> Result<UxOutput, String> {
    let start = raw.find('{').ok_or("no JSON object found")?;
    let end = raw.rfind('}').ok_or("no closing brace")?;
    if end < start {
        return Err("malformed object bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
}

fn ux_of(u: &UxOutput) -> UxDesign {
    UxDesign {
        user_flow: u.user_flow.clone(),
        screens: u.screens.clone(),
        component_states: u.component_states.clone(),
        responsive_notes: u.responsive_notes.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, AgentRequest, SandboxStatus};
    use crate::state::ProjectState;
    use crate::use_cases::{AddTicketInput, AddTicketUseCase};
    use coxagent_domain::{Complexity, Priority, TechnicalDesign, TicketType};
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
            s.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    struct Canned(String);
    #[async_trait::async_trait]
    impl AgentEnginePort for Canned {
        fn id(&self) -> &'static str {
            "canned"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: self.0.clone(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
                engine: String::new(),
            })
        }
    }

    /// Seed a UI feature that already has a technical design (SA done), pending.
    async fn seed_ui_with_technical(store: &Arc<MemStore>) -> TicketId {
        AddTicketUseCase::new(Arc::clone(store))
            .execute(AddTicketInput {
                ticket_type: TicketType::Feature,
                title: "UI feature".to_owned(),
                description: String::new(),
                priority: Priority::High,
                complexity: Complexity::Small,
                has_ui: true,
                acceptance_criteria: Vec::new(),
            })
            .await
            .expect("seed");
        let mut s = store.load().await.expect("load");
        let id = s.tickets[0].id().clone();
        s.tickets[0]
            .set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("tech");
        store.save(&s).await.expect("save");
        id
    }

    fn uc(store: Arc<MemStore>, out: &str) -> RunPdUseCase<MemStore, Canned> {
        RunPdUseCase::new(
            store,
            Arc::new(Canned(out.to_owned())),
            Config::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn authors_ux_and_readies_ui_feature() {
        let store = Arc::new(MemStore::default());
        seed_ui_with_technical(&store).await;
        let out = r#"{"user_flow":"open, type, send","screens":["compose"],"component_states":["empty","sending"],"responsive_notes":"stacks on mobile"}"#;
        let id = uc(Arc::clone(&store), out).execute().await.expect("run");
        assert!(id.is_some());
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Ready);
        let ux = state.tickets[0].design().ux.as_ref().expect("ux");
        assert_eq!(ux.screens, vec!["compose".to_owned()]);
    }

    #[tokio::test]
    async fn nothing_needing_ux_returns_none() {
        let store = Arc::new(MemStore::default());
        assert!(uc(store, "{}").execute().await.expect("run").is_none());
    }

    #[test]
    fn renderable_svg_accepts_well_formed() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="300"><rect x="40" y="52" width="120" height="80" fill="#101017"/><text x="40" y="80">Login</text></svg>"##;
        assert!(renderable_svg(svg));
    }

    #[test]
    fn renderable_svg_rejects_html_entity() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg"><text x="40" y="52">A &middot; B</text></svg>"##;
        assert!(!renderable_svg(svg));
    }

    #[test]
    fn renderable_svg_rejects_unquoted_attribute() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg"><text x=40 y=52>Login</text></svg>"##;
        assert!(!renderable_svg(svg));
    }

    #[test]
    fn renderable_svg_rejects_empty_stub() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg"></svg>"##;
        assert!(!renderable_svg(svg));
    }

    #[test]
    fn renderable_svg_accepts_numeric_entity() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg"><text x="40" y="52">A &#183; B</text></svg>"##;
        assert!(renderable_svg(svg));
    }
}
