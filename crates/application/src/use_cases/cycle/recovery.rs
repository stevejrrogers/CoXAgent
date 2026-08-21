// Part of the cycle module split by concern — see cycle/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Queue recovery and ops monitoring: unsticking a drained queue, the clean-base gate, and the deployed-app watchdog.

use super::*;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Recovery trips when the queue is at 2× the WIP limit (min 8) — past that
    /// point normal cycles can never drain it, so the team must switch to
    /// merge-only work.
    pub(super) fn recovery_threshold(&self) -> usize {
        (self.config.git.max_open_prs as usize * 2).max(8)
    }

    /// Merge-queue RECOVERY: entered automatically when the queue blows past
    /// [`Self::recovery_threshold`]. While active, cycles do merge/conflict work
    /// ONLY — no BA proposals, no TEST bug-filing (they just re-discover bugs
    /// whose fixes are stuck in the queue), no new branches. On entry: announce
    /// in #agents, reset the per-PR fix-attempt brakes so parked PRs get retried,
    /// and close obsolete "Resolve merge conflict on PR #N" resolver-PRs (that
    /// anti-pattern is exactly what piled the queue up). Exits, with an
    /// announcement, once the queue is back under the WIP limit.
    pub(super) async fn run_queue_recovery(&self, open: Option<usize>) -> bool {
        let Some(open) = open else { return false };
        let limit = self.config.git.max_open_prs as usize;
        let vi = self.config.workflow.language.is_vi();
        if open < self.recovery_threshold() {
            // Below the trip point. If we were recovering and are now under the
            // WIP limit, declare recovery over.
            if open <= limit {
                let msg = if vi {
                    format!("✅ Queue đã hồi phục — còn {open} PR mở (limit {limit}). Team quay lại làm việc bình thường.")
                } else {
                    format!("✅ Merge queue recovered — {open} open PR(s) (limit {limit}). Back to normal work.")
                };
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    if s.queue_recovery {
                        s.queue_recovery = false;
                        s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                    }
                    Ok(())
                })
                .await;
            }
            return false;
        }
        let msg = if vi {
            format!(
                "🚨 RECOVERY MODE: {open} PR đang mở (ngưỡng {}). Từ giờ mỗi cycle chỉ merge + gỡ \
                 conflict — không code mới, không file bug mới (bug cũ chưa merge thì test lại chỉ \
                 đẻ trùng). Đóng các PR 'resolve conflict' mồ côi. Queue về dưới {limit} là team \
                 chạy lại bình thường.",
                self.recovery_threshold()
            )
        } else {
            format!(
                "🚨 RECOVERY MODE: {open} open PRs (threshold {}). Cycles now do merge/conflict \
                 work only — no new code, no new bug filing. Obsolete resolver-PRs get closed. \
                 Normal work resumes under {limit} open PRs.",
                self.recovery_threshold()
            )
        };
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if !s.queue_recovery {
                s.queue_recovery = true;
                // Give every parked PR another shot under the new regime.
                s.pr_fix_attempts.clear();
                s.post_comment("SM", &msg, None);
                s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            }
            Ok(())
        })
        .await;
        self.report("SM", &format!("recovery: draining {open} open PRs"));
        // SM highlights the drain status EVERY recovery cycle — the team (and
        // any human watching chat) always knows how many conflicts stand
        // between them and new feature work.
        if let Some(forge) = &self.forge {
            if let Ok(prs) = forge.list_open_prs().await {
                let conflicted = prs.iter().filter(|p| !p.mergeable).count();
                let status = if vi {
                    format!(
                        "🔧 Recovery: còn {open} PR mở, {conflicted} dính conflict. Luật: xử hết \
                         conflict TRƯỚC rồi mới design/feature mới — DEV fix 8 conflict/cycle \
                         (cũ nhất trước), SA re-review và merge ngay khi xanh."
                    )
                } else {
                    format!(
                        "🔧 Recovery: {open} open PRs, {conflicted} conflicting. Law: conflicts \
                         are cleared BEFORE any new design/feature work — DEV fixes 8 per cycle \
                         (oldest first), SA re-reviews and merges as soon as they're green."
                    )
                };
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    if s.queue_recovery {
                        s.post_chat_in("SM", &status, crate::state::AGENTS_CHANNEL, Vec::new());
                    }
                    Ok(())
                })
                .await;
            }
        }
        // Resolver-PRs ("Resolve merge conflict on PR #N") are the anti-pattern
        // that inflated the queue — conflicts are fixed on the ORIGINAL branch by
        // address_pr_feedback, so these are pure noise. Close them.
        if let Some(forge) = &self.forge {
            if let Ok(prs) = forge.list_open_prs().await {
                for p in prs
                    .iter()
                    .filter(|p| p.title.contains("Resolve merge conflict on PR #"))
                {
                    if forge.close_pr(p.number).await.is_ok() {
                        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                            s.seen_closed_prs.insert(p.number);
                            Ok(())
                        })
                        .await;
                        self.log_git(&format!(
                            "recovery: closed obsolete resolver PR #{} ({})",
                            p.number, p.title
                        ))
                        .await;
                    }
                }
            }
        }
        true
    }

    /// A restructure is planned or underway: architecture refactor mode is on,
    /// or the PO's sprint goal reads like a refactor/migration.
    pub(super) async fn clean_base_required(&self) -> bool {
        let Ok(state) = self.store.load().await else {
            return false;
        };
        if state.refactor_mode {
            return true;
        }
        let goal = state
            .sprint
            .as_ref()
            .map(|s| s.goal.to_lowercase())
            .unwrap_or_default();
        [
            "refactor",
            "restructure",
            "migrat",
            "tái cấu trúc",
            "cấu trúc lại",
        ]
        .iter()
        .any(|k| goal.contains(k))
    }

    /// Ops/SRE monitor: once the app has been deployed, ping its published port
    /// each leader cycle. On an outage, file exactly one high-priority bug and
    /// alert the chat; on recovery, announce it. State-tracked so it never spams.
    pub(super) async fn ops_monitor(&self) {
        use coxagent_domain::ticket::{Complexity, Priority, TicketType};
        if !self.config.workflow.ops_monitor {
            return;
        }
        let Some(port) = self.config.deploy.host_port else {
            return;
        };
        let Some(deploy) = &self.deploy else {
            return;
        };
        let Ok(state) = self.store.load().await else {
            return;
        };
        // Only meaningful once something has actually been deployed.
        if state.history.is_empty() {
            return;
        }
        let was_down = state.ops_down;
        let healthy = deploy.health(port).await.unwrap_or(true);
        if !healthy && !was_down {
            let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
            let _ = adder
                .execute(crate::use_cases::AddTicketInput {
                    ticket_type: TicketType::Bug,
                    title: format!("App is DOWN — no response on port {port}"),
                    description: "The Ops monitor found the deployed app not accepting \
                                  connections. Check the container/logs for a crash and restore \
                                  service."
                        .to_owned(),
                    priority: Priority::High,
                    complexity: Complexity::Medium,
                    has_ui: false,
                    acceptance_criteria: vec![format!("App answers on 127.0.0.1:{port} again")],
                })
                .await;
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                s.ops_down = true;
                Ok(())
            })
            .await;
            self.notify(
                "ops_down",
                format!(
                    "App is DOWN — nothing responding on port {port}. Filed a high-priority bug."
                ),
            )
            .await;
        } else if healthy && was_down {
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                s.ops_down = false;
                Ok(())
            })
            .await;
            self.notify(
                "ops_up",
                format!("App recovered — responding on port {port} again."),
            )
            .await;
        }
    }
}
