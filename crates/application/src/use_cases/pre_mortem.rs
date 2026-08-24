//! `RunPreMortemUseCase` — CXA-F019 pre-mortem risk analysis.
//!
//! Before work starts on a ticket, this one-shot action assesses its anticipated
//! failure modes. `Small` tickets are cheap to reason about deterministically:
//! dependency blockers and over-long descriptions are scored by code, not by an
//! agent. Anything larger delegates to one engine run whose JSON findings are
//! parsed and persisted; if the agent is unavailable or its output unusable the
//! result degrades gracefully to [`PreMortemResult::Unavailable`] rather than
//! failing the whole cycle — a missing analysis must never brick a runner.

use crate::error::{AppError, PortError};
use crate::ports::outbound::{mutate_state, AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use coxagent_domain::{RiskEntry, RiskSeverity, Role};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Result of running a pre-mortem pass: either concrete findings were produced,
/// or the analysis was unavailable (agent failure / unparseable output).
#[derive(Debug)]
pub enum PreMortemResult {
    Completed(PreMortemOutcome),
    Unavailable(String),
}

/// The findings produced for one ticket and whether they came from deterministic
/// heuristics (`true`) or an agent run (`false`).
#[derive(Debug)]
pub struct PreMortemOutcome {
    pub findings: Vec<RiskEntry>,
    pub deterministic: bool,
}

/// One finding item as returned by the engine's JSON array.
#[derive(Debug, Deserialize)]
struct FindingJson {
    #[serde(default)]
    description: String,
    #[serde(default = "default_severity")]
    severity: String,
}

fn default_severity() -> String {
    "medium".to_owned()
}

/// Word count used by the scope heuristic.
fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Deterministic heuristic findings for a ticket (AC4). Only meaningful for
/// complexity=Small tickets; larger ones go through an agent instead.
fn heuristic(ticket: &coxagent_domain::Ticket) -> Vec<RiskEntry> {
    let mut out = Vec::new();
    if !ticket.depends_on().is_empty() {
        out.push(RiskEntry {
            description: format!(
                "blocked by {} dependent ticket(s); work may stall until they land",
                ticket.depends_on().len()
            ),
            severity: RiskSeverity::High,
        });
    }
    if word_count(ticket.description()) > 500 {
        out.push(RiskEntry {
            description: "description exceeds 500 words — long Small-ticket scope tends to \
                hide unclear requirements"
                .to_owned(),
            severity: RiskSeverity::Medium,
        });
    }
    out
}

/// Map an engine severity string onto [`RiskSeverity`], defaulting unknown values
/// to Medium.
fn parse_severity(s: &str) -> RiskSeverity {
    match s.trim().to_ascii_lowercase().as_str() {
        "high" => RiskSeverity::High,
        "low" => RiskSeverity::Low,
        _ => RiskSeverity::Medium,
    }
}

/// Parse engine stdout containing a JSON array of finding objects into domain
/// [`RiskEntry`]s. Tolerates prose around the array and per-item missing fields.
fn parse_findings(raw_output: &str) -> Result<Vec<RiskEntry>, String> {
    let start = raw_output.find('[').ok_or("no JSON array found")?;
    let end = raw_output.rfind(']').ok_or("no closing bracket")?;
    if end < start {
        return Err("malformed array bounds".to_owned());
    }
    let items: Vec<FindingJson> =
        serde_json::from_str(&raw_output[start..=end]).map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .filter(|f| !f.description.trim().is_empty())
        .map(|f| RiskEntry {
            description: f.description,
            severity: parse_severity(&f.severity),
        })
        .collect())
}

/// One-shot pre-mortem analysis before commitment. Mirrors `RunSaUseCase`'s
/// shape (store + engine + work_dir), but needs no config or selection logic —
/// it targets exactly one ticket by id.
pub struct RunPreMortemUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunPreMortemUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
        }
    }

    /// Run a pre-mortem pass on one ticket.
    ///
    /// # Errors
    /// [`AppError`] only when the store itself fails to load/save or the ticket
    /// cannot be found — an agent being unavailable degrades to
    /// [`PreMortemResult::Unavailable`] instead.
    pub async fn execute(
        &self,
        id: coxagent_domain::TicketId,
    ) -> Result<PreMortemResult, AppError> {
        let state = self.store.load().await?;
        let Some(ticket) = state.ticket(&id).cloned() else {
            return Err(PortError::NotFound(format!("ticket {id} missing")).into());
        };

        if ticket.complexity() == coxagent_domain::Complexity::Small {
            let findings = heuristic(&ticket);
            let target_id_for_persist = id;
            mutate_state(self.store.as_ref(), |st| {
                st.ticket_mut(&target_id_for_persist)
                    .ok_or_else(|| {
                        PortError::Corrupt(format!("ticket {target_id_for_persist} vanished"))
                    })?
                    .set_pre_mortem(Role::System, findings.clone())
                    .map_err(|e| PortError::Corrupt(e.to_string()))
            })
            .await?;
            return Ok(PreMortemResult::Completed(PreMortemOutcome {
                findings,
                deterministic: true,
            }));
        }

        let request = self.build_request(&id);
        let outcome = match self.engine.run(request).await {
            Ok(o) if o.succeeded() => o,
            _ => return Ok(Self::unavailable()),
        };
        match parse_findings(&outcome.stdout) {
            Ok(findings) => {
                let target_id_for_persist = id;
                mutate_state(self.store.as_ref(), |st| {
                    st.ticket_mut(&target_id_for_persist)
                        .ok_or_else(|| {
                            PortError::Corrupt(format!("ticket {target_id_for_persist} vanished"))
                        })?
                        .set_pre_mortem(Role::System, findings.clone())
                        .map_err(|e| PortError::Corrupt(e.to_string()))
                })
                .await?;
                Ok(PreMortemResult::Completed(PreMortemOutcome {
                    findings,
                    deterministic: false,
                }))
            }
            Err(_) => Ok(Self::unavailable()),
        }
    }

    fn build_request(&self, id: &coxagent_domain::TicketId) -> AgentRequest {
        AgentRequest {
            role: Role::System,
            system_prompt: prompts::system_prompt(prompts::SA),
            task_prompt: format!(
                "Perform a pre-mortem risk analysis on ticket {id}. Enumerate the likely \
                 failure modes and their severity before work starts. Output ONLY a JSON array \
                 of objects with fields \"description\" (string) and \"severity\" (one of \
                 \"high\", \"medium\", \"low\"). No prose."
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(120),
            escalation_level: 0,
            label: Some(id.to_string()),
        }
    }

    fn unavailable() -> PreMortemResult {
        PreMortemResult::Unavailable("pre-mortem unavailable - review technical design".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
    use crate::state::ProjectState;
    use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};
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

    struct Canned {
        stdout: String,
        exit_code: Option<i32>,
    }
    #[async_trait::async_trait]
    impl AgentEnginePort for Canned {
        fn id(&self) -> &'static str {
            "canned"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: self.stdout.clone(),
                stderr: String::new(),
                exit_code: self.exit_code,
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
                engine: String::new(),
            })
        }
    }

    fn tid(id: &str) -> TicketId {
        TicketId::new(id).expect("id")
    }

    /// Seed a store whose main ticket references `deps`; each dependency is also
    /// added as a leaf ticket so ProjectState::validate() passes on save.
    fn seed(main_id: &str, complexity: Complexity, desc: &str, deps: &[&str]) -> Arc<MemStore> {
        let mut tickets = Vec::new();
        for d in deps {
            tickets.push(
                Ticket::new(
                    tid(d),
                    TicketType::Feature,
                    "dep",
                    "",
                    Priority::Medium,
                    Complexity::Small,
                    false,
                )
                .expect("ticket"),
            );
        }
        let mut main = Ticket::new(
            tid(main_id),
            TicketType::Feature,
            "main",
            desc,
            Priority::Medium,
            complexity,
            false,
        )
        .expect("ticket");
        for d in deps {
            main.add_dependency(Role::Sa, tid(d)).expect("dep");
        }
        tickets.push(main);
        let state = ProjectState {
            tickets,
            ..ProjectState::default()
        };
        Arc::new(MemStore {
            state: Mutex::new(state),
        })
    }

    fn uc(store: Arc<MemStore>, out: &str) -> RunPreMortemUseCase<MemStore, Canned> {
        RunPreMortemUseCase::new(
            store,
            Arc::new(Canned {
                stdout: out.to_owned(),
                exit_code: Some(0),
            }),
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn small_with_dependency_is_deterministic_blocked() {
        let store = seed("CXA-F001", Complexity::Small, "small work", &["CXA-F000"]);
        let id = tid("CXA-F001");
        let res = uc(Arc::clone(&store), "[]").execute(id).await.expect("run");
        let PreMortemResult::Completed(out) = res else {
            panic!("expected Completed");
        };
        assert!(out.deterministic);
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].severity, RiskSeverity::High);
        assert!(out.findings[0].description.contains("blocked"));
    }

    #[tokio::test]
    async fn small_leaf_is_deterministic_empty() {
        let store = seed("CXA-F002", Complexity::Small, "no deps", &[]);
        let id = tid("CXA-F002");
        let res = uc(Arc::clone(&store), "[]").execute(id).await.expect("run");
        let PreMortemResult::Completed(out) = res else {
            panic!("expected Completed");
        };
        assert!(out.deterministic);
        assert!(out.findings.is_empty());
    }

    #[tokio::test]
    async fn medium_with_valid_json_persists_non_deterministic() {
        let store = seed("CXA-F003", Complexity::Medium, "medium work", &[]);
        let id = tid("CXA-F003");
        let out_json = r#"[{"description":"api may change","severity":"high"},{"description":"timing","severity":"low"}]"#;
        let res = uc(Arc::clone(&store), out_json)
            .execute(id.clone())
            .await
            .expect("run");
        let PreMortemResult::Completed(out) = res else {
            panic!("expected Completed");
        };
        assert!(!out.deterministic);
        assert_eq!(out.findings.len(), 2);
        assert_eq!(out.findings[0].severity, RiskSeverity::High);

        // Persisted on the store.
        let state = store.load().await.expect("load");
        let t = state.ticket(&id).expect("ticket exists");
        assert_eq!(t.pre_mortem().len(), 2);
    }

    #[tokio::test]
    async fn medium_agent_failure_is_unavailable_and_not_persisted() {
        let store = seed("CXA-F004", Complexity::Medium, "medium work", &[]);
        let id = tid("CXA-F004");
        let failing = Canned {
            stdout: "engine exploded".to_owned(),
            exit_code: Some(1),
        };
        let uc =
            RunPreMortemUseCase::new(Arc::clone(&store), Arc::new(failing), PathBuf::from("/tmp"));
        let res = uc.execute(id.clone()).await.expect("run");
        assert!(matches!(res, PreMortemResult::Unavailable(_)));

        // Nothing persisted.
        let state = store.load().await.expect("load");
        assert_eq!(state.ticket(&id).expect("ticket").pre_mortem().len(), 0);
    }

    #[test]
    fn word_count_boundary_is_strict_greater_than_five_hundred() {
        let exact = vec!["word"; 500].join(" ");
        assert_eq!(word_count(&exact), 500);
        // Exactly 500 does not fire the scope heuristic...
        assert!(!heuristic(&feature_ticket(&exact))
            .iter()
            .any(|r| r.severity == RiskSeverity::Medium));

        let over = vec!["word"; 501].join(" ");
        assert_eq!(word_count(&over), 501);
        // ...but one past it does.
        assert!(heuristic(&feature_ticket(&over))
            .iter()
            .any(|r| r.description.contains("500 words")));
    }

    fn feature_ticket(desc: &str) -> Ticket {
        Ticket::new(
            tid("CXA-F900"),
            TicketType::Feature,
            "boundary",
            desc,
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("ticket")
    }
}
