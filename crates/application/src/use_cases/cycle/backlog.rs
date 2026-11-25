// Part of the cycle module split by concern — see cycle/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Backlog hygiene: dedup, team-note promotion, and the periodic tech-debt sweep.

use super::*;

/// The cadence between scheduled debt sweeps: every tenth leader cycle files one
/// tech-debt chore if none is already open.
///
/// Today the LIVE tick decision lives at the call site in mod.rs (`% 10 == 0`);
/// per CXA-C001 that call site stays untouched to preserve behaviour byte-for-
/// byte, so outside test builds nothing references this constant directly. It is
/// still kept here as a single source of truth for [`should_file_debt_sweep`]
/// so the tests cannot silently drift from what production intends.
#[cfg_attr(not(test), allow(dead_code))]
const SWEEP_EVERY_CYCLES: u64 = 10;

/// Whether a still-in-flight "Debt sweep ..." chore already occupies the board —
/// i.e. one whose status has not reached a terminal outcome yet (`Pending`,
/// `Ready`, `Open`, ...). A new sweep must not be filed until it terminates,
/// otherwise every tenth cycle would pile up an endless stack of debt chores.
#[must_use]
fn has_open_chore(state: &crate::state::ProjectState) -> bool {
    use coxagent_domain::ticket::Status;
    state.tickets.iter().any(|t| {
        t.title().starts_with("Debt sweep")
            && !matches!(
                t.status(),
                Status::Done | Status::Documented | Status::Verified | Status::Rejected
            )
    })
}

/// Whether a previously filed "Debt sweep ..." chore was REJECTED — the team
/// looked at the debt and opted out of paying it this time, or an automated
/// gate marked it non-actionable. A rejected prior sweep must suppress any NEW
/// cycle-numbered sweep: re-filing after a rejection just stacks one rejected
/// chore per tick forever (127 and counting in production), because the debt
/// it points at never gets cleared by a chore that is rejected instead of
/// done. The cadence still logs the tick in `sweeps_done` for the scorecard,
/// but no new ticket is spawned.
#[must_use]
fn has_rejected_sweep(state: &crate::state::ProjectState) -> bool {
    use coxagent_domain::ticket::Status;
    state
        .tickets
        .iter()
        .any(|t| t.title().starts_with("Debt sweep") && t.status() == Status::Rejected)
}

/// Pure decision rule for whether THIS tick should produce a scheduled tech-debt
/// chore, computed from persisted state + the current cycle number alone — no IO
/// behind it — so its cases read plainly in one place and unit-test independently:
///
/// * true on a sweep-th tick (`cycle % 10 == 0`) with no open chore and no
///   previously-rejected sweep;
/// * false on any other (non-sweep-th) tick;
/// * false while any still-open "Debt sweep ..." chore exists;
/// * true again once that prior chore COMPLETES (`Done`/`Verified`/`Documented`);
/// * false forever once a prior sweep was REJECTED — re-filing after a rejection
///   only piles up rejected chores, so the cadence records the tick but never
///   spawns another ticket.
// Production keeps this rule split across `file_debt_sweep`: the "%10 tick" gate
// lives at its call site and each blocking case ends in a DIFFERENT side effect
// (`sweeps_done` records a handled-but-not-filed cycle only on some paths), so a
// single bool cannot reproduce them losslessly. This predicate therefore exists as
// an authored, regression-locked statement of ALL four acceptance cases together,
// exercised by unit tests below rather than reaching out of test builds.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn should_file_debt_sweep(state: &crate::state::ProjectState, cycle: u64) -> bool {
    if cycle % SWEEP_EVERY_CYCLES != 0 {
        return false;
    }
    if state.sweeps_done.contains(&cycle) {
        return false;
    }
    !has_open_chore(state) && !has_rejected_sweep(state)
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Reject duplicate tickets (same normalised title, or a semantic near-match)
    /// that haven't started real work yet, keeping the earliest, and call it out
    /// on the thread — so a blind re-proposal never gets designed or built twice.
    pub(super) async fn dedup_backlog(&self) {
        use coxagent_domain::ticket::Status;
        let Ok(mut state) = self.store.load().await else {
            return;
        };
        // Kept tickets carry their title token-set so we can flag not only exact
        // re-titles but semantic near-duplicates (paraphrases) too.
        let mut first: std::collections::HashMap<String, TicketId> =
            std::collections::HashMap::new();
        let mut kept: Vec<(TicketId, std::collections::HashSet<String>)> = Vec::new();
        let mut dupes: Vec<(TicketId, TicketId, String)> = Vec::new();
        for t in &state.tickets {
            if t.status() == Status::Rejected {
                continue;
            }
            let key = crate::parsing::normalize_title(t.title());
            if key.is_empty() {
                continue;
            }
            if let Some(orig) = first.get(&key) {
                dupes.push((t.id().clone(), orig.clone(), t.title().to_owned()));
                continue;
            }
            // Semantic pass: reject when the title overlaps an earlier ticket's
            // heavily (Jaccard ≥ 0.6 on content tokens), e.g. "Disappearing
            // Messages" vs "Auto-deleting messages".
            let tokens = crate::parsing::title_tokens(t.title());
            if let Some((orig, _)) = kept
                .iter()
                .find(|(_, seen)| crate::parsing::jaccard(&tokens, seen) >= 0.6)
            {
                dupes.push((t.id().clone(), orig.clone(), t.title().to_owned()));
                continue;
            }
            first.insert(key, t.id().clone());
            kept.push((t.id().clone(), tokens));
        }
        let mut rejected = Vec::new();
        for (dup, orig, title) in dupes {
            if let Some(t) = state.ticket_mut(&dup) {
                // Only reject work that hasn't been picked up yet.
                if matches!(t.status(), Status::Pending | Status::Ready | Status::Open)
                    && t.transition_to(coxagent_domain::Role::User, Status::Rejected)
                        .is_ok()
                {
                    rejected.push((dup, orig, title));
                }
            }
        }
        if rejected.is_empty() {
            return;
        }
        let vi = self.config.workflow.language.is_vi();
        for (dup, orig, title) in &rejected {
            let msg = if vi {
                format!(
                    "Heads up — {dup} trùng với {orig} (\"{title}\"). Từ chối {dup} để khỏi làm \
                     trùng. BA nhớ kiểm tra backlog trước khi đề xuất."
                )
            } else {
                format!(
                    "Heads up — {dup} duplicates {orig} (\"{title}\"). Rejecting {dup} so we don't \
                     build the same thing twice. BA, please check the backlog before proposing."
                )
            };
            state.post_comment("SM", &msg, Some(dup.to_string()));
        }
        state.log_activity(
            "SM",
            &format!("rejected {} duplicate ticket(s)", rejected.len()),
            None,
        );
        let _ = self.store.save(&state).await;
    }

    /// Post the daily digest into the team chat, at most once per UTC day (the
    /// marker lives in state, so restarts and multiple operators can't repeat
    /// it). The very first run only stamps the day — no digest of nothing.
    /// Append a promoted team lesson to the repo's `CLAUDE.md` under a
    /// dedicated section — versioned via git, read by the engine on EVERY
    /// machine. Dedupes on exact text. Returns whether anything was written.
    pub(super) async fn promote_team_note(&self, note: &str) -> bool {
        use std::fmt::Write as _;
        const HEADER: &str = "## Team learnings (auto-promoted by memory hygiene)";
        let Some(files) = &self.files else {
            return false;
        };
        let path = self.work_dir.join("CLAUDE.md");
        let cur = files.read(&path).await.unwrap_or_default();
        if cur.contains(note) {
            return false;
        }
        let mut next = cur.clone();
        if !next.contains(HEADER) {
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            next.push('\n');
            next.push_str(HEADER);
            next.push('\n');
        }
        let _ = writeln!(next, "- {note}");
        files.write(&path, &next).await
    }

    /// Where the claude CLI keeps its per-project auto-memory for this
    /// codebase: `~/.claude/projects/<work_dir with '/'→'-'>/memory`.
    pub(super) fn engine_memory_dir(&self) -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let slug = self.work_dir.to_string_lossy().replace('/', "-");
        let dir = std::path::Path::new(&home)
            .join(".claude/projects")
            .join(slug)
            .join("memory");
        dir.is_dir().then_some(dir)
    }

    /// File the periodic tech-debt chore, carved from evidence gathered this
    /// cycle rather than from vague habit: lint delta against the prior clippy
    /// baseline and, when workspace files are readable, source modules lacking
    /// doc headers — each finding lands in the ticket's description and as a
    /// concrete acceptance criterion so whoever picks it up knows what "done"
    /// means. Deduped by title prefix AND by recorded cycle number, so a restart
    /// never re-files for a sweep already run.
    pub(super) async fn file_debt_sweep(&self, cycle: u64) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        // Restart dedup - SILENT on purpose: a cycle already swept must not be
        // re-recorded as handled again. In-flight-chore blocking happens below,
        // where it still ends in `note_swept` so the window stays closed.
        if state.sweeps_done.contains(&cycle) {
            return;
        }
        let open_exists = has_open_chore(&state);
        // A prior sweep that ended REJECTED suppresses any new sweep chore —
        // re-filing after a rejection just stacks rejected chores every tick
        // (the debt it tracks is never cleared by a chore that is rejected
        // instead of done). Record the tick as handled; spawn no ticket.
        if has_rejected_sweep(&state) {
            self.note_swept(cycle).await;
            return;
        }
        if open_exists && !state.debt_signals.is_empty() {
            self.note_swept(cycle).await;
            return;
        }

        // Gather measurements through ports — lint from DeployPort, module docs
        // from workspace files (absent read access → those signals stay zero).
        let lint_now = match &self.deploy {
            Some(d) => d.lint_report(&self.work_dir).await.ok().flatten(),
            None => None,
        };
        let mut missing_docs = 0usize;
        if let Some(files) = &self.files {
            missing_docs = self.missing_module_docs(files.as_ref()).await;
        }

        // Only file when there is actual action to take: some lint regression or
        // at least one module short of a doc header. A clean codebase earns no chore.
        let signals = crate::use_cases::cycle::debt_sweep::assemble_signals(
            lint_now.as_ref(),
            state.clippy_baseline,
            missing_docs,
        );
        if signals.is_empty() {
            self.note_swept(cycle).await;
            return;
        }
        if open_exists {
            self.note_swept(cycle).await;
            return;
        }

        let accepted = crate::use_cases::cycle::debt_sweep::acceptance_lines(&signals);
        let desc = crate::use_cases::cycle::debt_sweep::describe(signals.len());
        self.persist_and_file(cycle, state, desc, accepted).await;
    }

    async fn note_swept(&self, cycle: u64) {
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            s.sweeps_done.push(cycle);
            Ok(())
        })
        .await;
    }

    /// Scan repo sources through the workspace-files port for modules lacking a
    /// doc header. Reads are done once per sweep; vendored/build dirs are skipped
    /// by the caller (the adapter reports everything below the root).
    async fn missing_module_docs(
        &self,
        files: &dyn crate::ports::outbound::WorkspaceFilesPort,
    ) -> usize {
        let paths = files.list_recursive(&self.work_dir).await;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for p in paths {
            let is_rs = p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "rs");
            if !is_rs {
                continue;
            }
            if let Some(src) = files.read(&p).await {
                pairs.push((p.to_string_lossy().to_string(), src));
            }
        }
        crate::use_cases::cycle::debt_sweep::count_modules_missing_docs(
            pairs
                .iter()
                .map(|(path, src)| (path.as_str(), src.as_str())),
        )
    }

    async fn persist_and_file(
        &self,
        cycle: u64,
        state: crate::state::ProjectState,
        desc: String,
        accepted: Vec<String>,
    ) {
        let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
        let Ok(id) = adder
            .execute(crate::use_cases::AddTicketInput {
                ticket_type: coxagent_domain::TicketType::Chore,
                title: format!("Debt sweep (cycle {cycle})"),
                description: desc,
                priority: coxagent_domain::ticket::Priority::Medium,
                complexity: coxagent_domain::ticket::Complexity::Medium,
                has_ui: false,
                acceptance_criteria: accepted,
                goal: None,
                service_tag: None,
            })
            .await
        else {
            return;
        };
        let signals = state.debt_signals;
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            s.debt_signals.clone_from(&signals);
            s.sweeps_done.push(cycle);
            s.log_activity("SM", "filed scheduled debt sweep", Some(id.to_string()));
            Ok(())
        })
        .await;
    }
}

#[cfg(test)]
mod should_file_debt_sweep_tests {
    use super::should_file_debt_sweep;
    use crate::state::ProjectState;
    use coxagent_domain::Role;
    use coxagent_domain::Status;
    use coxagent_domain::{Complexity, Priority};

    fn sweep_ticket(target: Status) -> coxagent_domain::Ticket {
        let mut t = coxagent_domain::Ticket::new(
            coxagent_domain::TicketId::new("COX-C001").expect("id"),
            coxagent_domain::TicketType::Chore,
            "Debt sweep".to_owned(),
            "pay down tech debt".to_owned(),
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("ticket");
        // Ready requires a design (DoR); give this fixture one so the whole
        // lifecycle to Done/Documented can be exercised.
        assert!(t
            .set_technical_design(
                Role::System,
                coxagent_domain::TechnicalDesign {
                    approach: "sweep".to_owned(),
                    files: vec![],
                    api_contract: String::new(),
                    data_changes: String::new(),
                    test_plan: "n/a".to_owned(),
                    alternatives: String::new(),
                }
            )
            .is_ok());
        if target == Status::Ready {
            assert!(t.transition_to(Role::System, Status::Ready).is_ok());
        } else if target == Status::InProgress {
            assert!(t.transition_to(Role::System, Status::Ready).is_ok());
            assert!(t.transition_to(Role::System, Status::InProgress).is_ok());
        } else if target == Status::Done {
            assert!(t.transition_to(Role::System, Status::Ready).is_ok());
            assert!(t.transition_to(Role::System, Status::InProgress).is_ok());
            assert!(t.transition_to(Role::System, Status::Done).is_ok());
        } else if target == Status::Documented {
            assert!(t.transition_to(Role::System, Status::Ready).is_ok());
            assert!(t.transition_to(Role::System, Status::InProgress).is_ok());
            assert!(t.transition_to(Role::System, Status::Done).is_ok());
            assert!(t.transition_to(Role::System, Status::Documented).is_ok());
        } else if target == Status::Rejected {
            assert!(t.transition_to(Role::System, Status::Rejected).is_ok());
        }
        t
    }

    fn state(tickets: Vec<coxagent_domain::Ticket>, done: Vec<u64>) -> ProjectState {
        ProjectState {
            tickets,
            sweeps_done: done,
            ..Default::default()
        }
    }

    #[test]
    fn sweep_files_on_a_sweep_th_tick_with_no_open_chore() {
        let state = ProjectState::default();
        assert!(should_file_debt_sweep(&state, 10));
    }

    #[test]
    fn sweep_does_not_file_on_a_non_sweep_th_tick() {
        let state = ProjectState::default();
        assert!(!should_file_debt_sweep(&state, 11));
    }
    #[test]
    fn sweep_is_blocked_while_a_pending_chore_is_open() {
        let t = sweep_ticket(Status::Pending);
        let state = state(vec![t], vec![]);
        assert!(!should_file_debt_sweep(&state, 10));
    }

    #[test]
    fn sweep_is_blocked_while_a_ready_chore_is_open() {
        let t = sweep_ticket(Status::Ready);
        let state = state(vec![t], vec![]);
        assert!(!should_file_debt_sweep(&state, 10));
    }

    #[test]
    fn sweep_is_blocked_while_an_in_progress_chore_is_open() {
        let t = sweep_ticket(Status::InProgress);
        let state = state(vec![t], vec![]);
        assert!(!should_file_debt_sweep(&state, 10));
    }

    #[test]
    fn sweep_files_again_once_the_prior_reaches_terminal_done() {
        let prior = sweep_ticket(Status::Done);
        // next tick (20) not yet swept; only cycle 10 was.
        let state = state(vec![prior], vec![10]);
        assert!(should_file_debt_sweep(&state, 20));
    }

    #[test]
    fn sweep_files_again_once_the_prior_reaches_terminal_documented() {
        let prior = sweep_ticket(Status::Documented);
        // next tick (20) not yet swept; only cycle 10 was.
        let state = state(vec![prior], vec![10]);
        assert!(should_file_debt_sweep(&state, 20));
    }

    #[test]
    fn sweep_is_suppressed_forever_after_a_rejected_prior_sweep() {
        // A rejected sweep means the team opted out; re-filing a fresh
        // cycle-numbered sweep just stacks rejected chores each tick. This is
        // the regression-lock for the production bug (127 rejected sweeps).
        let prior = sweep_ticket(Status::Rejected);
        let st1 = state(vec![prior.clone()], vec![10]);
        assert!(!should_file_debt_sweep(&st1, 20));
        // ...and it stays suppressed on every later tick too.
        let later = state(vec![prior], vec![10, 20, 30, 40, 50]);
        assert!(!should_file_debt_sweep(&later, 60));
        assert!(!should_file_debt_sweep(&later, 100));
    }

    #[test]
    fn sweep_for_an_already_swept_exact_cycle_is_not_refiled() {
        let mut s = ProjectState::default();
        s.sweeps_done.push(10);
        assert!(!should_file_debt_sweep(&s, 10));
    }

    #[test]
    fn an_unrelated_open_ticket_does_not_block_the_sweep() {
        // An unrelated ticket that happens to be open must NOT suppress
        // filing; only an open chore of that kind closes the window.
        let t = coxagent_domain::Ticket::new(
            coxagent_domain::TicketId::new("COX-C002").expect("id"),
            coxagent_domain::TicketType::Feature,
            "Build a thing".to_owned(),
            "not debt".to_owned(),
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("ticket");
        let state = state(vec![t], vec![]);
        assert!(should_file_debt_sweep(&state, 10));
    }
}
