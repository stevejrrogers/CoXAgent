//! `RunMilestonesUseCase` — the PO lays out the product milestones from the
//! goal and current backlog. Milestones are the roadmap targets that sprints
//! work toward; each has a `target_version` that marks it reached. Authored
//! once, then EXTENDED when the team ships past the last target — a roadmap
//! that stops at a version already released is a roadmap nobody is steering by.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::state::Milestone;
use coxagent_domain::{Role, SemVer};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct MilestoneOut {
    name: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    target_version: String,
}

/// Authors the milestone roadmap when absent.
pub struct RunMilestonesUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    context: String,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunMilestonesUseCase<S, E> {
    pub fn new(
        store: Arc<S>,
        engine: Arc<E>,
        config: Config,
        work_dir: PathBuf,
        context: String,
    ) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            context,
            files: None,
        }
    }

    /// Attach workspace file access for prompt context blocks.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    /// Author the milestone plan when the project has a backlog but no
    /// milestones, and extend it once every milestone has been reached.
    /// Returns `true` when milestones were written this pass.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or unparseable output.
    #[allow(clippy::too_many_lines)] // one linear pass; splitting hurts readability
    pub async fn execute(&self) -> Result<bool, AppError> {
        let state = self.store.load().await?;
        if state.tickets.is_empty() {
            return Ok(false);
        }
        let had = state.milestones.len();
        // Extend rather than sit on a finished roadmap: the last target being
        // released means every milestone is reached and the team has nothing
        // left to steer by.
        let extending = !state.milestones.is_empty();
        if extending && !roadmap_reached(&state.milestones, &state.current_version) {
            return Ok(false);
        }
        let _ = self.config.engine.resolve(Role::Po);

        let backlog: Vec<String> = state
            .tickets
            .iter()
            .map(|t| format!("- {}", t.title()))
            .collect();
        let current = state.current_version.to_string();
        let backlog_text = backlog.join("\n");
        // A roadmap written without knowing what already shipped re-promises it.
        let knowledge = crate::prompts::knowledge_block(
            self.files.as_deref(),
            &state.docs,
            &state.tickets,
            &self.work_dir,
            &format!("{} {}", self.context, backlog_text),
            "",
        )
        .await;
        let backlog_block = format!("{backlog_text}{knowledge}");
        let task_prompt = if extending {
            let shipped: Vec<String> = state
                .milestones
                .iter()
                .map(|m| format!("- {} (v{})", m.name, m.target_version))
                .collect();
            format!(
                "Product goal:\n{}\n\nThe roadmap below is FULLY REACHED — the product now \
                 ships at v{current}:\n{}\n\nRemaining backlog:\n{}\n\nPlan the NEXT 3-5 \
                 sequential milestones that carry the product forward from here. Do not repeat \
                 outcomes already delivered above. Respond with ONLY a JSON array, each item \
                 exactly: {{\"name\": string, \"goal\": string, \"target_version\": \"MAJOR.MINOR.0\"}} \
                 with strictly increasing target_version, every one GREATER than {current}.",
                self.context,
                shipped.join("\n"),
                backlog_block
            )
        } else {
            format!(
                "Product goal:\n{}\n\nCurrent backlog:\n{}\n\nLay out 3-5 sequential product \
                 milestones from earliest to latest. Each milestone is a meaningful, shippable \
                 outcome that may take several sprints. Respond with ONLY a JSON array, each item \
                 exactly: {{\"name\": string, \"goal\": string, \"target_version\": \"MAJOR.MINOR.0\"}} \
                 with strictly increasing target_version (e.g. 0.2.0, 0.5.0, 1.0.0).",
                self.context,
                backlog_block
            )
        };
        let request = AgentRequest {
            role: Role::Po,
            system_prompt: prompt_system(),
            task_prompt,
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(300),
            escalation_level: 0,
            label: None,
        };

        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "PO milestones engine failed: {}",
                outcome.failure_detail()
            ))
            .into());
        }
        let parsed = parse(&outcome.stdout)
            .map_err(|e| PortError::Corrupt(format!("PO milestones output: {e}")))?;
        let cur = state.current_version.clone();
        let milestones: Vec<Milestone> = parsed
            .into_iter()
            .filter(|m| !m.name.trim().is_empty())
            .map(|m| Milestone {
                name: m.name.trim().to_owned(),
                goal: m.goal.trim().to_owned(),
                target_version: m.target_version.trim().to_owned(),
                goal_complete: false,
                fulfilled: false,
            })
            // An extension target at or below the shipped version is reached
            // the moment it is written — drop it rather than grow a dead roadmap.
            .filter(|m| !extending || SemVer::parse(&m.target_version).is_ok_and(|v| v > cur))
            .take(6)
            .collect();
        if milestones.is_empty() {
            return Ok(false);
        }

        let mut state = self.store.load().await?;
        if state.milestones.len() != had {
            return Ok(false); // raced with another writer
        }
        let n = milestones.len();
        if extending {
            state.milestones.extend(milestones);
            state.log_activity("PO", &format!("extended roadmap with {n} milestones"), None);
            state.post_comment(
                "PO",
                &format!(
                    "Roadmap reached at v{cur} — planned {n} new milestones to carry us \
                     forward. Sprints ladder up to these now."
                ),
                None,
            );
        } else {
            state.milestones = milestones;
            state.log_activity("PO", &format!("planned {n} milestones"), None);
            state.post_comment(
                "PO",
                &format!(
                    "Roadmap set — {n} milestones from here to launch. Each one is a real \
                      shippable target; sprints ladder up to them."
                ),
                None,
            );
        }
        self.store.save(&state).await?;
        Ok(true)
    }
}

/// Whether every milestone target has been released. Unparseable targets are
/// treated as not reached, so a malformed roadmap never triggers an extension.
fn roadmap_reached(milestones: &[Milestone], current: &SemVer) -> bool {
    milestones
        .iter()
        .all(|m| SemVer::parse(&m.target_version).is_ok_and(|target| *current >= target))
}

fn prompt_system() -> String {
    crate::prompts::system_prompt(crate::prompts::PO)
}

fn parse(raw: &str) -> Result<Vec<MilestoneOut>, String> {
    let start = raw.find('[').ok_or("no JSON array found")?;
    let end = raw.rfind(']').ok_or("no closing bracket")?;
    if end < start {
        return Err("malformed array bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
    use crate::state::ProjectState;
    use async_trait::async_trait;
    use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};
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
        async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
            state.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = state.clone();
            Ok(())
        }
    }

    struct CannedEngine {
        stdout: String,
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl AgentEnginePort for CannedEngine {
        fn id(&self) -> &'static str {
            "canned"
        }
        async fn run(&self, req: AgentRequest) -> Result<AgentOutcome, PortError> {
            self.seen.lock().expect("lock").push(req.task_prompt);
            Ok(AgentOutcome {
                stdout: self.stdout.clone(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
            })
        }
    }

    fn seeded(version: &str, milestones: Vec<(&str, &str)>) -> Arc<MemStore> {
        let mut st = ProjectState {
            current_version: SemVer::parse(version).expect("version"),
            milestones: milestones
                .into_iter()
                .map(|(name, target)| Milestone {
                    name: name.to_owned(),
                    goal: "g".to_owned(),
                    target_version: target.to_owned(),
                    goal_complete: false,
                    fulfilled: false,
                })
                .collect(),
            ..ProjectState::default()
        };
        st.tickets.push(
            Ticket::new(
                TicketId::new("COX-F001").expect("id"),
                TicketType::Feature,
                "A feature",
                "desc",
                Priority::Medium,
                Complexity::Medium,
                false,
            )
            .expect("ticket"),
        );
        Arc::new(MemStore {
            state: Mutex::new(st),
        })
    }

    fn uc(
        store: Arc<MemStore>,
        engine: Arc<CannedEngine>,
    ) -> RunMilestonesUseCase<MemStore, CannedEngine> {
        RunMilestonesUseCase::new(
            store,
            engine,
            Config::default(),
            PathBuf::from("/tmp"),
            "ship it".to_owned(),
        )
    }

    #[tokio::test]
    async fn extends_the_roadmap_once_every_target_has_shipped() {
        let store = seeded("0.5.2", vec![("Early", "0.1.0"), ("Budget caps", "0.5.0")]);
        let engine = Arc::new(CannedEngine {
            stdout: r#"[{"name":"Multi-repo","goal":"g","target_version":"0.7.0"},
                        {"name":"GA","goal":"g","target_version":"1.0.0"}]"#
                .to_owned(),
            seen: Mutex::new(Vec::new()),
        });
        assert!(uc(Arc::clone(&store), Arc::clone(&engine))
            .execute()
            .await
            .expect("run"));
        let names: Vec<String> = store
            .state
            .lock()
            .expect("lock")
            .milestones
            .iter()
            .map(|m| m.name.clone())
            .collect();
        assert_eq!(names, ["Early", "Budget caps", "Multi-repo", "GA"]);
        let prompt = engine.seen.lock().expect("lock")[0].clone();
        assert!(
            prompt.contains("FULLY REACHED") && prompt.contains("0.5.2"),
            "the PO must be told what already shipped: {prompt}"
        );
    }

    #[tokio::test]
    async fn leaves_an_unfinished_roadmap_alone() {
        let store = seeded("0.5.2", vec![("Early", "0.1.0"), ("GA", "1.0.0")]);
        let engine = Arc::new(CannedEngine {
            stdout: r#"[{"name":"Nope","goal":"g","target_version":"2.0.0"}]"#.to_owned(),
            seen: Mutex::new(Vec::new()),
        });
        assert!(!uc(Arc::clone(&store), Arc::clone(&engine))
            .execute()
            .await
            .expect("run"));
        assert_eq!(store.state.lock().expect("lock").milestones.len(), 2);
        assert!(engine.seen.lock().expect("lock").is_empty(), "no LLM call");
    }

    #[tokio::test]
    async fn drops_extension_targets_at_or_below_the_shipped_version() {
        let store = seeded("0.5.2", vec![("Budget caps", "0.5.0")]);
        let engine = Arc::new(CannedEngine {
            // 0.4.0 is already released; only 0.9.0 is a real target.
            stdout: r#"[{"name":"Stale","goal":"g","target_version":"0.4.0"},
                        {"name":"Real","goal":"g","target_version":"0.9.0"}]"#
                .to_owned(),
            seen: Mutex::new(Vec::new()),
        });
        assert!(uc(Arc::clone(&store), Arc::clone(&engine))
            .execute()
            .await
            .expect("run"));
        let names: Vec<String> = store
            .state
            .lock()
            .expect("lock")
            .milestones
            .iter()
            .map(|m| m.name.clone())
            .collect();
        assert_eq!(names, ["Budget caps", "Real"]);
    }
}
