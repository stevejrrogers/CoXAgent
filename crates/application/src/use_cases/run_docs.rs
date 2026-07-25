//! `RunDocsUseCase` — the DOCS agent. Documents one `Done` feature (writing a
//! user guide into the codebase) and moves it to `Documented`. Runs after TEST
//! so only completed work is documented.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::selection::documentable_candidates;
use coxagent_domain::{Role, Status, TicketId};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Runs one documentation pass.
pub struct RunDocsUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    worker: String,
    phase: Option<crate::use_cases::runner::PhaseReporter>,
    context: Option<String>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunDocsUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            worker: String::new(),
            phase: None,
            context: None,
        }
    }

    #[must_use]
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// Set this runner's identity (`account@host`) so the DOCS stage is claimed
    /// per-ticket for parallel-safe documentation across concurrent runners.
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

    /// Document the next `Done` feature. Returns its id, or `None` when there's
    /// nothing to document.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or an unexpected transition error.
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        let state = self.store.load().await?;
        let worker = if self.worker.is_empty() {
            "local".to_owned()
        } else {
            self.worker.clone()
        };
        let now = crate::state::now_rfc3339();
        let mut chosen = None;
        for cand in documentable_candidates(&state) {
            if self.store.claim_stage(&cand, "docs", &worker, &now).await? {
                chosen = Some(cand);
                break;
            }
        }
        let Some(id) = chosen else {
            return Ok(None);
        };
        if let Some(p) = &self.phase {
            p(Some(("DOCS".to_owned(), id.to_string())));
        }
        let ticket_type = state.ticket(&id).map_or(
            coxagent_domain::TicketType::Feature,
            coxagent_domain::Ticket::ticket_type,
        );
        let title = state
            .ticket(&id)
            .map_or("", coxagent_domain::Ticket::title)
            .to_owned();

        // The standard space for this ticket type, plus the sub-folders that
        // already exist under it — so the agent reuses one rather than inventing
        // a redundant sibling. This keeps the tree tidy instead of "lung tung".
        let space = crate::state::standard_doc_folder(ticket_type);
        let prefix = format!("{space}/");
        let existing_subs: Vec<String> = state
            .doc_folders
            .iter()
            .filter_map(|f| f.strip_prefix(&prefix))
            .filter(|s| !s.contains('/'))
            .map(ToOwned::to_owned)
            .collect();

        let _choice = self.config.engine.resolve(Role::Docs);
        let context_block = self
            .context
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(|c| {
                format!("\n\n## Project context (goal, stack — doc within this framing):\n{c}\n")
            })
            .unwrap_or_default();
        let repo_map = prompts::repo_map_block(&self.work_dir, self.config.workflow.token_saver);
        let outcome = self
            .engine
            .run(AgentRequest {
                role: Role::Docs,
                system_prompt: prompts::system_prompt(prompts::DOCS),
                task_prompt: format!(
                    "{}{context_block}{repo_map}",
                    build_docs_prompt(&id, &title, space, &existing_subs)
                ),
                work_dir: self.work_dir.clone(),
                timeout: Duration::from_secs(900),
            })
            .await?;
        if !outcome.succeeded() {
            self.store.release_stage(&id, "docs", &worker).await.ok();
            return Err(PortError::Backend(format!(
                "DOCS engine failed on {id}: {}",
                outcome.stderr.trim()
            ))
            .into());
        }

        // Split the leading `FOLDER: <topic>` hint off the body, then map it to a
        // clean sub-folder — reusing an existing one when it matches, dropping it
        // when empty/junk so a page never creates a stray folder.
        let (sub, doc_body) = parse_folder_hint(&outcome.stdout);
        let sub = sub.and_then(|s| sanitize_subfolder(&s, &existing_subs));
        let folder = match &sub {
            Some(s) => format!("{space}/{s}"),
            None => space.to_owned(),
        };
        let category = crate::state::doc_category_of(space);

        // Surface the documentation in the Wiki: one page per documented feature,
        // so the knowledge base actually fills up as the team ships (not just
        // markdown buried in the codebase).
        let body = if doc_body.trim().len() > 40 {
            doc_body.trim().to_owned()
        } else {
            format!("Documentation for **{title}** ({id}). See the codebase docs for details.")
        };
        let has_sub = sub.is_some();
        // Atomic read-modify-write with retry (parallel-safe).
        crate::ports::outbound::mutate_state(self.store.as_ref(), |state| {
            state.ensure_standard_folders();
            let ticket = state
                .ticket_mut(&id)
                .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished")))?;
            ticket
                .transition_to(Role::Docs, Status::Documented)
                .map_err(|e| PortError::Corrupt(e.to_string()))?;
            if has_sub {
                state.add_doc_folder(&folder);
            }
            state.upsert_doc(
                &format!("feat-{id}"),
                &folder,
                category,
                &title,
                &body,
                "DOCS",
            );
            Ok(())
        })
        .await?;
        if let Some(p) = &self.phase {
            p(None);
        }
        Ok(Some(id))
    }
}

/// Build the DOCS task prompt, asking the agent to first pick the best
/// sub-folder for the page under its space — reusing an existing one when it
/// fits, only proposing a new concise topic when none do, so the Wiki stays
/// organised rather than sprouting redundant folders.
fn build_docs_prompt(id: &TicketId, title: &str, space: &str, existing: &[String]) -> String {
    let subs = if existing.is_empty() {
        "(none yet)".to_owned()
    } else {
        existing.join(", ")
    };
    format!(
        "Document ticket {id}: {title}\n\n\
         This page lives in the \"{space}\" Wiki space. Existing sub-folders there: {subs}.\n\
         On the FIRST line, output exactly `FOLDER: <topic>` naming the best home for this page: \
         reuse one of the existing sub-folders when it fits; only propose a NEW short topic \
         (2-3 words, Title Case, e.g. \"Authentication\", \"Messaging\") when none fit; or write \
         `FOLDER: -` to leave it at the space root. Do NOT invent redundant or one-off folders.\n\
         Then, from the next line on, write the documentation in Markdown."
    )
}

/// Split a leading `FOLDER: <topic>` line off the agent output. Returns the
/// raw topic (if present and not the `-` sentinel) and the remaining body.
fn parse_folder_hint(stdout: &str) -> (Option<String>, String) {
    let trimmed = stdout.trim_start();
    let Some(rest) = trimmed
        .strip_prefix("FOLDER:")
        .or_else(|| trimmed.strip_prefix("Folder:"))
    else {
        return (None, stdout.to_owned());
    };
    let (line, body) = rest.split_once('\n').unwrap_or((rest, ""));
    let topic = line.trim();
    let hint = if topic.is_empty() || topic == "-" {
        None
    } else {
        Some(topic.to_owned())
    };
    (hint, body.to_owned())
}

/// Clean an agent-proposed sub-folder into a safe, tidy name, or `None` to file
/// at the space root. Keeps letters/digits/spaces/hyphens only, collapses
/// whitespace, caps the length, and — case-insensitively — reuses an existing
/// sub-folder name so "auth" and "Auth" never split into two.
fn sanitize_subfolder(raw: &str, existing: &[String]) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return None;
    }
    let name: String = if joined.len() > 40 {
        joined
            .chars()
            .take(40)
            .collect::<String>()
            .trim_end()
            .to_owned()
    } else {
        joined
    };
    // Reuse an existing folder that matches case-insensitively, so "auth" and
    // "Auth" never split the tree into two near-duplicate folders.
    Some(
        existing
            .iter()
            .find(|e| e.eq_ignore_ascii_case(&name))
            .cloned()
            .unwrap_or(name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::AgentOutcome;
    use crate::state::ProjectState;
    use coxagent_domain::{Complexity, Priority, TechnicalDesign, Ticket, TicketType};
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

    struct OkEngine;
    #[async_trait::async_trait]
    impl AgentEnginePort for OkEngine {
        fn id(&self) -> &'static str {
            "ok"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: "wrote docs".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
            })
        }
    }

    fn done_feature(id: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t");
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("d");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.transition_to(Role::DevFeature, Status::InProgress)
            .expect("claim");
        t.transition_to(Role::DevFeature, Status::Done)
            .expect("done");
        t
    }

    #[test]
    fn parses_folder_hint_and_body() {
        let (sub, body) = parse_folder_hint("FOLDER: Messaging\n# Guide\nbody");
        assert_eq!(sub.as_deref(), Some("Messaging"));
        assert_eq!(body.trim(), "# Guide\nbody");
        // Sentinel and missing header both yield no sub-folder.
        assert_eq!(parse_folder_hint("FOLDER: -\nx").0, None);
        assert_eq!(parse_folder_hint("no header here").0, None);
    }

    #[test]
    fn sanitize_reuses_and_cleans() {
        let existing = vec!["Authentication".to_owned()];
        // Case-insensitive reuse: "auth"→ existing "Authentication"? No — only an
        // exact case-insensitive match reuses; "authentication" does.
        assert_eq!(
            sanitize_subfolder("authentication", &existing).as_deref(),
            Some("Authentication")
        );
        // Junk characters are stripped; a clean topic survives.
        assert_eq!(
            sanitize_subfolder("Push/Notifications!!", &[]).as_deref(),
            Some("Push Notifications")
        );
        // Empty after cleaning → no folder.
        assert_eq!(sanitize_subfolder("///", &[]), None);
    }

    #[tokio::test]
    async fn documents_done_feature() {
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                tickets: vec![done_feature("F001")],
                ..ProjectState::default()
            }),
        });
        let uc = RunDocsUseCase::new(
            Arc::clone(&store),
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
        );
        let id = uc.execute().await.expect("run");
        assert_eq!(id.expect("some").as_str(), "F001");
        assert_eq!(
            store.load().await.expect("load").tickets[0].status(),
            Status::Documented
        );
    }

    #[tokio::test]
    async fn nothing_to_document_is_none() {
        let store = Arc::new(MemStore::default());
        let uc = RunDocsUseCase::new(
            store,
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
        );
        assert!(uc.execute().await.expect("run").is_none());
    }
}
