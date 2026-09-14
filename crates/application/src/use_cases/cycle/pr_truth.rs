// Part of the cycle module split by concern — see cycle/mod.rs.
//! PR-truth sweep: a rescue PR whose target is gone rescues nothing.
//!
//! The feedback lane opens "fix: unblock PR #N" branches; when #N is later
//! merged or closed by another path, the rescue keeps squatting a WIP slot
//! against `max_open_prs` (seen live: #387 held a slot for hours after its
//! target #334 closed). Once a day, close every rescue PR whose target is no
//! longer open.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};

/// Extract the target number from a rescue-PR title ("… unblock PR #N …").
fn rescue_target(title: &str) -> Option<u64> {
    let (_, rest) = title.split_once("unblock PR #")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Leader-only, date-gated (`daily_jobs["pr_truth"]`). Best-effort: any
    /// forge failure skips the day, never the cycle.
    pub(super) async fn pr_truth_sweep(&self) {
        let Some(forge) = self.forge.as_ref() else {
            return;
        };
        let Ok(state) = self.store.load().await else {
            return;
        };
        let today = crate::state::now_rfc3339()[..10].to_owned();
        if state.daily_jobs.get("pr_truth") == Some(&today) {
            return;
        }
        let Ok(open) = forge.list_open_prs().await else {
            return;
        };
        let open_numbers: std::collections::BTreeSet<u64> =
            open.iter().map(|pr| pr.number).collect();
        let mut closed: Vec<u64> = Vec::new();
        for pr in &open {
            let Some(target) = rescue_target(&pr.title) else {
                continue;
            };
            if open_numbers.contains(&target) {
                continue;
            }
            let note = format!(
                "PR-truth sweep: this rescue targets PR #{target}, which is no \
                 longer open — the branch's purpose is void and it was holding \
                 a WIP slot. Reopen if the underlying work is still needed."
            );
            let _ = forge.comment_pr(pr.number, &note).await;
            if forge.close_pr(pr.number).await.is_ok() {
                closed.push(pr.number);
            }
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.daily_jobs.insert("pr_truth".to_owned(), today.clone());
            for n in &closed {
                s.log_activity(
                    "SYSTEM",
                    &format!("pr-truth: closed orphaned rescue PR #{n} (target gone)"),
                    None,
                );
            }
            Ok(())
        })
        .await;
        if !closed.is_empty() {
            tracing::warn!(
                "pr-truth: closed {} orphaned rescue PR(s): {:?}",
                closed.len(),
                closed
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rescue_target;

    #[test]
    fn rescue_target_parses_the_standard_title_and_rejects_others() {
        assert_eq!(rescue_target("fix: unblock PR #334"), Some(334));
        assert_eq!(rescue_target("fix: unblock PR #12 (retry)"), Some(12));
        assert_eq!(rescue_target("feat(CXA-F1): add river"), None);
        assert_eq!(rescue_target("fix: unblock PR #"), None);
    }
}
