// Part of the cycle module split by concern — see cycle/mod.rs.
//! Ship-truth sweep: "shipped" must mean "on main".
//!
//! The ticket lifecycle (DEV→TEST→DOCS) and the PR merge lane are separate;
//! in the bad weeks the review queue jammed, rescue closed PRs unmerged, and
//! the status machine still marched tickets to Documented — a dashboard full
//! of shipped work whose diff never landed (the CXA-B093/B096 class). Once a
//! day, cross-check every shipped-status ticket against the base branch and
//! demote the lies back to Pending/Open so the team re-does them for real.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use coxagent_domain::Status;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Leader-only, date-gated (`daily_jobs["ship_truth"]`). Best-effort: any
    /// git failure skips the day, never the cycle.
    pub(super) async fn ship_truth_sweep(&self) {
        let Some(git) = self.git.as_ref() else {
            return;
        };
        let Ok(state) = self.store.load().await else {
            return;
        };
        let today = crate::state::now_rfc3339()[..10].to_owned();
        if state.daily_jobs.get("ship_truth") == Some(&today) {
            return;
        }
        let base = {
            let b = self.config.git.default_branch.trim();
            format!("origin/{}", if b.is_empty() { "main" } else { b })
        };
        // One fetch so the check reads today's main, not last week's.
        let (ok, _) = git
            .raw(&self.work_dir, &["fetch", "origin", "--quiet"])
            .await;
        if !ok {
            return;
        }
        let shipped: Vec<String> = state
            .tickets
            .iter()
            .filter(|t| {
                matches!(
                    t.status(),
                    Status::Done | Status::Documented | Status::Verified
                )
            })
            .map(|t| t.id().to_string())
            .collect();
        // A ticket whose PR is still OPEN is in flight, not a ghost — the
        // sweep reopening it while DEV resolves merge conflicts would fork the
        // same work twice (seen live with CXA-F229).
        let in_flight = |id: &str| {
            state
                .open_prs
                .iter()
                .any(|pr| pr.title.contains(id) || pr.head.contains(id))
        };
        let mut ghosts: Vec<String> = Vec::new();
        for id in shipped {
            if in_flight(&id) {
                continue;
            }
            let (ok, out) = git
                .raw(
                    &self.work_dir,
                    &["log", &base, "--grep", &id, "--oneline", "-1"],
                )
                .await;
            if ok && out.trim().is_empty() {
                ghosts.push(id);
            }
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.daily_jobs.insert("ship_truth".to_owned(), today.clone());
            for id in &ghosts {
                let Some(t) = s.tickets.iter_mut().find(|t| t.id().to_string() == *id) else {
                    continue;
                };
                let to = match t.status() {
                    Status::Verified => Status::Open,
                    _ => Status::Pending,
                };
                if t.transition_to(coxagent_domain::Role::System, to).is_err() {
                    continue;
                }
                s.log_activity(
                    "SYSTEM",
                    "ship-truth: claimed shipped but absent from main — reopened",
                    Some(id.clone()),
                );
            }
            if !ghosts.is_empty() {
                let msg = format!(
                    "🔎 Ship-truth sweep: {} ticket(s) were marked shipped but their \
                     diff is not on {base} — reopened for an honest re-run: {}",
                    ghosts.len(),
                    ghosts.join(", ")
                );
                s.post_comment("SM", &msg, None);
            }
            Ok(())
        })
        .await;
        if !ghosts.is_empty() {
            tracing::warn!(
                "ship-truth: reopened {} ticket(s) absent from {base}: {}",
                ghosts.len(),
                ghosts.join(", ")
            );
        }
    }
}
