// Part of the cycle module split by concern — see cycle/mod.rs.
//! PO daily pass: the Product Owner earns its keep once a day.
//!
//! Everything else the PO does is event-driven (goal gate, priorities,
//! rejections), so on a healthy day the role sat idle and the roadmap
//! drifted: milestones nobody re-read, stale backlog rows nobody demoted.
//! Once a day (date-gated like ship-truth), the PO:
//!
//! 1. **Roadmap reconciliation** — compares each milestone against what
//!    actually shipped (release version, open scope) and posts a short
//!    reconciliation to Scrum: fulfilled milestones named, drifted targets
//!    flagged for revision.
//! 2. **Backlog triage** — calls out High-priority tickets sitting outside
//!    the sprint (tickets carry no creation timestamp, so age-based demotion
//!    is deliberately NOT guessed at).

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use coxagent_domain::{Priority, SemVer, Status};

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Leader-only, date-gated (`daily_jobs["po_daily"]`). Best-effort: any
    /// failure skips the day, never the cycle.
    pub(super) async fn po_daily_pass(&self) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let today = crate::state::now_rfc3339()[..10].to_owned();
        if state.daily_jobs.get("po_daily") == Some(&today) {
            return;
        }
        // ---- 1. Roadmap reconciliation (pure summary; revising targets
        // stays a PO/human decision, this surfaces the drift). A milestone
        // whose target version the product has already passed while still
        // "in progress" is exactly the drift the user kept spotting by hand
        // (targets v2.23/v2.26 open at v2.29) — call it out explicitly.
        let released = match &self.files {
            Some(files) => files
                .read(&self.work_dir.join("Cargo.toml"))
                .await
                .as_deref()
                .and_then(super::parse_cargo_version),
            None => None,
        };
        let passed = |target: &str| -> bool {
            match (&released, SemVer::parse(target)) {
                (Some(rel), Ok(t)) => SemVer::parse(rel)
                    .is_ok_and(|r| r >= t),
                _ => false,
            }
        };
        let mut lines: Vec<String> = Vec::new();
        for m in &state.milestones {
            let status = if m.goal_complete {
                "✅ complete"
            } else if passed(&m.target_version) {
                "⚠️ target version already shipped — mark complete or move the target"
            } else {
                "🚧 in progress"
            };
            lines.push(format!(
                "• {} (target v{}) — {status}",
                m.name, m.target_version
            ));
        }
        // ---- 2. Backlog triage. ----
        let committed: Vec<String> = state
            .sprint
            .as_ref()
            .map(|sp| sp.committed.iter().map(ToString::to_string).collect())
            .unwrap_or_default();
        let stuck_high: Vec<String> = state
            .tickets
            .iter()
            .filter(|t| {
                t.priority() == Priority::High
                    && matches!(t.status(), Status::Pending | Status::Open)
                    && !committed.contains(&t.id().to_string())
            })
            .map(|t| t.id().to_string())
            .collect();
        let msg = po_daily_message(&lines, &stuck_high);
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            s.daily_jobs.insert("po_daily".to_owned(), today.clone());
            s.post_comment("PO", &msg, None);
            Ok(())
        })
        .await;
    }
}

/// The Scrum post. Pure so the wording is testable.
fn po_daily_message(milestones: &[String], stuck_high: &[String]) -> String {
    let mut msg = String::from("📋 PO daily — roadmap & backlog:\n");
    if milestones.is_empty() {
        msg.push_str("Roadmap: no milestones defined yet.\n");
    } else {
        msg.push_str("Roadmap:\n");
        for l in milestones {
            msg.push_str(l);
            msg.push('\n');
        }
    }
    if !stuck_high.is_empty() {
        use std::fmt::Write as _;
        let _ = writeln!(
            msg,
            "⚠️ High-priority but NOT in the sprint: {} — commit or re-prioritise.",
            stuck_high.join(", ")
        );
    }
    if stuck_high.is_empty() {
        msg.push_str("Backlog: clean — nothing stale, no stranded High tickets.\n");
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::po_daily_message;

    #[test]
    fn message_names_stranded_high_tickets_and_the_clean_case() {
        let msg = po_daily_message(
            &["• M1 (target v1.0) — 🚧 in progress".into()],
            &["CXA-F002".into()],
        );
        assert!(msg.contains("CXA-F002"));
        assert!(msg.contains("M1"));
        let clean = po_daily_message(&[], &[]);
        assert!(clean.contains("clean"));
    }
}
