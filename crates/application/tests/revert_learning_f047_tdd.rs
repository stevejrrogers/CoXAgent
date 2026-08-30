//! TDD tests for CXA-F047 — Merged-then-reverted work learning loop.
//!
//! These tests encode the ticket's acceptance criteria and FAIL until the
//! behaviour exists. They drive the one seam that already exists and that the
//! analysis must live in: [`RunCycleUseCase::run_cycle`], the leader pass that
//! re-runs every cycle ("when analysis re-runs…") and already owns the state
//! store and the [`GitPort`]. Fixtures are built only from types the codebase
//! has today: [`DeployRecord`] deploy history, a shipped [`Ticket`], and a git
//! double that answers `raw` the way git answers a `--pretty` request.
//!
//! The port conversation the git double defines (this IS the contract the
//! implementation must code against): the scan reads git exclusively through
//! [`GitPort::raw`]; for any `log` invocation the double answers `Ok` with one
//! line per commit, emitting exactly the requested `--pretty=` codes in git's
//! order (`%H`/`%h` sha, `%cI`/`%aI` RFC3339 date, `%s` subject), tab
//! (`\x1f`) separated; with no known format code it answers subjects only.
//! A scan that wants to apply the N-day window therefore HAS to request a
//! date code — the window cannot be applied to subjects alone.
//!
//! DELIBERATELY NOT FABRICATED (design gaps — see the ASK SA note at the
//! bottom): the criteria's "persistent revert event", the human approve /
//! dismiss decision on each event, the next-cycle planning-weight adjustment
//! fed by approved events, and the "N configured days" knob reference state,
//! decisions and config that do not exist anywhere in the codebase (no
//! `ProjectState` field, no decision record, no weight input, no config
//! section), and there is no SA design document for CXA-F047 to name them.
//! Tests here assert what existing types can honestly observe: the analysis
//! surfacing a detected revert as 'reverted work' on the Overview feed
//! ([`ActivityEntry`] — the dashboard's "what are the agents doing" view),
//! linked to the shipping ticket, attributed to the shipping ticket's agent
//! role, idempotent across re-runs, with pre-existing data untouched, and all
//! git reads arriving through [`GitPort::raw`]. Persistence, approve/dismiss
//! and planning weights stay untested until the SA specifies their shape —
//! guessing a field name here would be fabrication, not TDD.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use coxagent_application::config::{Config, GitConfig};
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, GitAuthor, GitPort, SandboxStatus, StateStorePort,
    SyncBase,
};
use coxagent_application::state::{now_rfc3339, ActivityEntry, DeployRecord, ProjectState};
use coxagent_application::use_cases::RunCycleUseCase;
use coxagent_application::PortError;
use coxagent_domain::{Complexity, Priority, Role, SemVer, Status, Ticket, TicketId, TicketType};
use std::path::Path;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Doubles — pure, in-memory, no IO beyond what the ports define.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MemStore {
    state: Mutex<ProjectState>,
}

#[async_trait]
impl StateStorePort for MemStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().unwrap().clone())
    }
    async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
        s.validate().map_err(PortError::Corrupt)?;
        *self.state.lock().unwrap() = s.clone();
        Ok(())
    }
}

/// One commit the scan can see in `git log`.
struct LogCommit {
    sha: &'static str,
    /// RFC3339 committer date — the only date shape the window math can trust.
    date: String,
    subject: String,
}

/// A git adapter that never leaves the test: records every `raw` invocation
/// (so the tests can prove the scan's git reads go through the port) and
/// answers `log` per the format-code contract in the module docs. Everything
/// else reads as "git did nothing", the port's own convention for doubles.
struct LogGit {
    raw_calls: Mutex<Vec<String>>,
    commits: Vec<LogCommit>,
}

impl LogGit {
    fn in_window_revert(ticket: &str) -> LogCommit {
        LogCommit {
            sha: "c0ffee1",
            date: now_rfc3339(),
            subject: format!("Revert \"feat({ticket}): add widget\""),
        }
    }

    /// Long before any sane window, under either anchor (deploy date or now).
    fn out_of_window_revert(ticket: &str) -> LogCommit {
        LogCommit {
            sha: "badc0de",
            date: "2020-01-01T00:00:00Z".to_owned(),
            subject: format!("Revert \"feat({ticket}): add widget\""),
        }
    }

    fn plain_feat_commit(ticket: &str) -> LogCommit {
        LogCommit {
            sha: "feedbee",
            date: now_rfc3339(),
            subject: format!("feat({ticket}): add widget"),
        }
    }
}

/// Emit one line per commit carrying exactly the requested pretty codes, in
/// git's order, tab-separated — the shape `git log --pretty=%H%x1f%cI%x1f%s`
/// really prints, so the double is faithful to the tool behind the port.
fn answer_log(spec: &str, commits: &[LogCommit]) -> String {
    let want_sha = spec.contains("%H") || spec.contains("%h");
    let want_date = spec.contains("%cI") || spec.contains("%aI");
    let want_subject = spec.contains("%s");
    // No known code requested: answer subjects, so a plain subject scan still
    // sees the world (and the date-carrying tests force the date request).
    let default_subjects = !want_sha && !want_date && !want_subject;
    commits
        .iter()
        .map(|c| {
            let mut parts: Vec<String> = Vec::new();
            if want_sha {
                parts.push(c.sha.to_owned());
            }
            if want_date {
                parts.push(c.date.clone());
            }
            if want_subject || default_subjects {
                parts.push(c.subject.clone());
            }
            parts.join("\u{1f}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl GitPort for LogGit {
    async fn raw(&self, _work_dir: &Path, args: &[&str]) -> (bool, String) {
        self.raw_calls.lock().unwrap().push(args.join("\u{1}"));
        if args.contains(&"log") {
            let spec = args
                .iter()
                .rev()
                .find(|a| a.starts_with("--pretty=") || a.starts_with("--format="))
                .and_then(|a| a.split('=').nth(1))
                .unwrap_or("");
            return (true, answer_log(spec, &self.commits));
        }
        (false, String::new())
    }

    async fn is_repo(&self, _: &Path) -> bool {
        true
    }
    async fn current_branch(&self, _: &Path) -> Result<String, PortError> {
        Ok("main".to_owned())
    }
    async fn checkout_branch(&self, _: &Path, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn commit_all(
        &self,
        _: &Path,
        _message: &str,
        _author: &GitAuthor,
    ) -> Result<Option<String>, PortError> {
        Ok(Some("deadbee".to_owned()))
    }
    async fn push(&self, _: &Path, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn sync_base(&self, _: &Path, _: &str) -> Result<SyncBase, PortError> {
        Ok(SyncBase::UpToDate)
    }
    async fn abort_merge(&self, _: &Path) -> Result<(), PortError> {
        Ok(())
    }
}

/// An engine that proposes nothing and reports nothing — the cycle's agent
/// phases are no-ops, so the only state change under test is the analysis.
struct Silent;

#[async_trait]
impl AgentEnginePort for Silent {
    fn id(&self) -> &'static str {
        "silent"
    }
    async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
        Ok(AgentOutcome {
            stdout: "[]".to_owned(),
            stderr: String::new(),
            exit_code: Some(0),
            usage: None,
            trace: String::new(),
            session_id: None,
            sandbox: SandboxStatus::default(),
            engine: String::new(),
            model: String::new(),
            attempts: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// Fixtures — every field from types the codebase actually has.
// ---------------------------------------------------------------------------

/// A feature the team shipped: ticket done, deploy recorded, git history
/// carrying both the merge and (in some worlds) a later revert of it.
fn shipped_feature_state(ticket: &str) -> ProjectState {
    let mut t = Ticket::new(
        TicketId::new(ticket).expect("valid ticket id"),
        TicketType::Feature,
        "add widget",
        "the widget feature",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    // The REAL lifecycle: a feature starts Pending; the SA authors the
    // technical design (the Pending→Ready gate), and only then is a
    // DEV-FEATURE claim legal.
    t.set_technical_design(Role::Sa, coxagent_domain::TechnicalDesign::default())
        .expect("SA authors the technical design");
    t.transition_to(Role::Sa, Status::Ready)
        .expect("the design gate readies a pending feature");
    t.claim(Role::DevFeature, "dev@host", "2026-08-19T00:00:00Z")
        .expect("ready ticket is claimable");
    t.transition_to(Role::DevFeature, Status::Done)
        .expect("dev may finish a claimed feature");
    ProjectState {
        current_version: SemVer::parse("1.2.3").unwrap(),
        history: vec![DeployRecord {
            version: SemVer::parse("1.2.3").unwrap(),
            ticket: TicketId::new(ticket).unwrap(),
            title: "add widget".to_owned(),
            at: now_rfc3339(),
        }],
        tickets: vec![t],
        ..ProjectState::default()
    }
}

fn config() -> Config {
    Config {
        git: GitConfig {
            enabled: true,
            ..GitConfig::default()
        },
        ..Config::default()
    }
}

type World = (Arc<MemStore>, Arc<LogGit>, tempfile::TempDir);

/// Boot the cycle over a shipped feature and a git history of `commits`.
fn world(commits: Vec<LogCommit>) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = shipped_feature_state("CXA-F041");
    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    let git = Arc::new(LogGit {
        raw_calls: Mutex::new(Vec::new()),
        commits,
    });
    (store, git, dir)
}

/// One leader pass — the analysis run. Boxed: the cycle future is huge.
async fn run_analysis(store: &Arc<MemStore>, git: &Arc<LogGit>, dir: &tempfile::TempDir, n: u64) {
    let uc = RunCycleUseCase::new(
        Arc::clone(store),
        Arc::new(Silent),
        config(),
        dir.path().to_path_buf(),
        "goal".to_owned(),
    )
    .with_git(Arc::clone(git) as Arc<dyn GitPort>);
    Box::pin(uc.run_cycle(n)).await;
}

/// The 'reverted work' signal the Overview feed shows, whatever else the cycle
/// logged around it.
fn reverted_work(state: &ProjectState) -> Vec<&ActivityEntry> {
    state
        .activity
        .iter()
        .filter(|a| a.action.to_lowercase().contains("reverted work"))
        .collect()
}

// ---------------------------------------------------------------------------
// AC1 — "Given at least one DeployRecord exists in history, scanning git log
// detects commits whose subject begins with Revert and links them to the
// matching ticket id within N configured days."
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac1_detects_a_revert_commit_and_links_it_to_the_shipping_ticket() {
    let (store, git, dir) = world(vec![
        LogGit::plain_feat_commit("CXA-F041"),
        LogGit::in_window_revert("CXA-F041"),
    ]);
    run_analysis(&store, &git, &dir, 1).await;

    let state = store.load().await.unwrap();
    let hits = reverted_work(&state);
    assert_eq!(
        hits.len(),
        1,
        "exactly one detected revert, got: {:?}",
        hits.iter().map(|h| &h.action).collect::<Vec<_>>()
    );
    assert_eq!(
        hits[0].ticket.as_deref(),
        Some("CXA-F041"),
        "the revert is linked to the matching ticket id from deploy history"
    );
}

#[tokio::test]
async fn ac1_reverts_outside_the_configured_window_are_not_detected() {
    let (store, git, dir) = world(vec![LogGit::out_of_window_revert("CXA-F041")]);
    run_analysis(&store, &git, &dir, 1).await;

    let state = store.load().await.unwrap();
    assert!(
        reverted_work(&state).is_empty(),
        "a revert committed far outside the N-day window is not flagged"
    );
}

// ---------------------------------------------------------------------------
// AC2 — "A detected revert creates a persistent revert event attributed to the
// shipping ticket's agent role, visible on Overview/board as 'reverted work'
// without changing any pre-existing data."
//
// The observable surface today is the Overview feed (ActivityEntry — the
// dashboard's "what are the agents doing" view). PERSISTENCE beyond that feed
// is a design gap (see module docs): no revert-event state exists to assert
// on, so it is deliberately not faked here.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac2_reverted_work_is_attributed_to_the_shipping_role_without_touching_existing_data() {
    let (store, git, dir) = world(vec![
        LogGit::plain_feat_commit("CXA-F041"),
        LogGit::in_window_revert("CXA-F041"),
    ]);
    // Snapshot the SEEDED data before the run — regenerating it here would
    // compare against a fresh `now_rfc3339()`, not against what was seeded.
    let before = store.load().await.unwrap();
    let (history_before, tickets_before) = (before.history.clone(), before.tickets.clone());
    run_analysis(&store, &git, &dir, 1).await;

    let state = store.load().await.unwrap();
    let hits = reverted_work(&state);
    assert_eq!(hits.len(), 1, "only the Revert commit is flagged");
    assert!(
        hits[0].agent.eq_ignore_ascii_case("DEV-FEATURE"),
        "attributed to the shipping ticket's agent role, got: {}",
        hits[0].agent
    );
    assert_eq!(
        state.history, history_before,
        "deploy history is untouched by the analysis"
    );
    assert_eq!(state.tickets, tickets_before, "tickets are untouched");
    assert_eq!(
        state.current_version,
        SemVer::parse("1.2.3").unwrap(),
        "the version is untouched"
    );
}

// ---------------------------------------------------------------------------
// AC4 — "When analysis re-runs after prior approval state, already-dismissed
// events are not re-flagged and do not double-count."
//
// The double-count half is observable: a second analysis pass must not flag
// the same revert again. The dismissed-half needs the approve/dismiss decision
// state, which does not exist — design gap, not faked (see module docs).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac4_rerunning_the_analysis_does_not_double_count_a_known_revert() {
    let (store, git, dir) = world(vec![LogGit::in_window_revert("CXA-F041")]);
    run_analysis(&store, &git, &dir, 1).await;
    run_analysis(&store, &git, &dir, 2).await;

    let state = store.load().await.unwrap();
    assert_eq!(
        reverted_work(&state).len(),
        1,
        "a second analysis pass must not re-flag the same revert"
    );
}

// ---------------------------------------------------------------------------
// AC5 — "The feature introduces no direct IO in application code — all git
// reads go through GitPort::raw via an adapter."
//
// Positive proof here: when a revert is there to find, the scan's git reads
// arrive as `raw` invocations carrying a `log` command. The standing guard
// for the negative half (no direct IO in application files) is
// crates/app/tests/hexagonal_gate.rs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac5_the_scan_reads_git_only_through_gitport_raw() {
    let (store, git, dir) = world(vec![LogGit::in_window_revert("CXA-F041")]);
    run_analysis(&store, &git, &dir, 1).await;

    let state = store.load().await.unwrap();
    assert!(
        !reverted_work(&state).is_empty(),
        "the analysis actually ran and detected the revert"
    );
    let calls = git.raw_calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|c| c.split('\u{1}').any(|arg| arg == "log")),
        "the scan's git reads go through GitPort::raw; raw calls were: {calls:?}"
    );
}

// ---------------------------------------------------------------------------
// AC3 — "A human can approve or dismiss each detected revert; only approved
// events feed back into next-cycle planning weight adjustments."
//
// NOT TESTED — design gap, not an oversight. Approving/dismissing needs a
// per-event human decision record and next-cycle planning needs a weight
// input; neither exists in the codebase (no ProjectState field, no API, no
// weight concept in selection/planning), and no SA design for CXA-F047 names
// them. Fabricating a field name here would test a guess, not the criteria.
//
// ASK SA: specify the persisted revert-event type + state field, the
// approve/dismiss decision record and where it lives, the planning-weight
// input approved events feed (which field, what adjustment), and the config
// knob for the N-day window — then these tests extend to AC3 honestly.
// ---------------------------------------------------------------------------
