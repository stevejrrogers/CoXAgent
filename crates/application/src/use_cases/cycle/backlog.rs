// Part of the cycle module split by concern — see cycle/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Backlog hygiene: dedup, team-note promotion, and the periodic tech-debt sweep.

use super::*;

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
        use coxagent_domain::ticket::Status;
        let Ok(state) = self.store.load().await else {
            return;
        };
        if state.sweeps_done.contains(&cycle) {
            return;
        }
        let open_exists = state.tickets.iter().any(|t| {
            t.title().starts_with("Debt sweep")
                && !matches!(
                    t.status(),
                    Status::Done | Status::Documented | Status::Verified | Status::Rejected
                )
        });
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
