//! Loop pause explainer (CXA-F384) — a pure verdict that names WHY the cycle
//! loop is stopped, since when, and what unblocks it.
//!
//! The loop stops four different ways (engine circuit-breaker, daily spend cap,
//! engine quota exhaustion, operator pause), but the dashboard only ever showed
//! a red dot. The operator's first question — "are the agents running?" — had
//! no answer in the UI.
//!
//! This is a pure precedence function over data the caller already holds, in the
//! same snapshot -> pure-decision shape as [`crate::liveness`] and
//! `use_cases/run_dev`: the handler assembles [`PauseInputs`] from persisted
//! state (`engine_incidents`, `WorkspaceRun` control attribution, spend totals)
//! and this module decides. Zero IO, so the hexagonal gate stays green.
//!
//! Precedence (most severe first, per the design):
//! `engine_breaker` > `budget_cap` > `engine_quota` > `stall` > `operator`.
//!
//! Honesty rule: nothing is ever guessed. An operator pause with no recorded
//! reason says so ("no reason recorded") rather than inventing one, and an
//! input that cannot be parsed simply does not qualify — the next reason down
//! the list gets its turn.

use serde::Serialize;

/// Why the loop is stopped, in the system's own wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseWhy {
    /// Two infra faults (or an auth-death) tripped the engine circuit breaker.
    EngineBreaker,
    /// The daily spend cap was reached.
    BudgetCap,
    /// Every engine is out of quota (`liveness::QUOTA_PAUSE_LINE`).
    EngineQuota,
    /// The liveness watchdog found a silent stall.
    Stall,
    /// A person pressed pause.
    Operator,
}

impl PauseWhy {
    /// The wire token the dashboard switches on.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EngineBreaker => "engine_breaker",
            Self::BudgetCap => "budget_cap",
            Self::EngineQuota => "engine_quota",
            Self::Stall => "stall",
            Self::Operator => "operator",
        }
    }
}

/// An open engine incident: the circuit breaker is holding the loop down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakerFault {
    /// Engine id (`claude`, `opencode`).
    pub engine: String,
    /// The decisive failure line, already trimmed.
    pub reason: String,
    /// RFC3339 when the incident opened.
    pub since: String,
}

/// The daily cap was reached: how much was spent, and against which cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetBreach {
    pub spent_usd: f64,
    pub cap_usd: f64,
}

/// The newest cycle event that evidenced a stall — the liveness predicate's own
/// input (a role and when it last moved), never a bare "stalled".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StallEvidence {
    /// Role label of the last cycle event (`DEV-BUG`).
    pub role: String,
    /// RFC3339 of that event.
    pub at: String,
}

/// A person paused the loop: who, when, and why (all may be empty — an
/// unrecorded reason stays unrecorded).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperatorPause {
    pub by: String,
    pub at: String,
    pub reason: String,
}

/// Everything the decision needs. The adapter fills it; the function never
/// reaches for anything itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PauseInputs {
    /// Runner mode label (`running` / `paused` / `stopped`). Only `paused`
    /// produces a card: a stopped loop is not a pause, and a running loop's
    /// zero-state is the header strip's job.
    pub mode: String,
    /// Seconds since the pause was stamped, when this process knows. `None`
    /// after a hub restart (the stamp is deliberately volatile).
    pub paused_since_secs: Option<u64>,
    pub breaker: Option<BreakerFault>,
    pub budget: Option<BudgetBreach>,
    /// Set when the newest cycle pause line is the quota wall.
    pub quota: bool,
    pub stall: Option<StallEvidence>,
    pub operator: Option<OperatorPause>,
}

/// The derived card. Serialized verbatim onto the existing `/runner` response.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PauseExplanation {
    pub paused: bool,
    /// `engine_breaker` | `budget_cap` | `engine_quota` | `stall` | `operator`,
    /// or `""` when nothing is paused.
    pub why: &'static str,
    /// One line the operator reads first.
    pub headline: String,
    /// The evidence behind the headline.
    pub detail: String,
    /// What unblocks it — actions or named navigation, in the order to try.
    pub unblocks: Vec<String>,
    /// Who paused it (empty when the loop paused itself).
    pub by: String,
    /// RFC3339 when it started (empty when unknown after a restart).
    pub at: String,
    /// Seconds since the pause; `0` when unknown.
    pub since_secs: u64,
    /// Every cause that applies, most severe first (the headline's cause is
    /// always `why`). Lets the card list the rest rather than hide them.
    pub others: Vec<&'static str>,
}

impl PauseExplanation {
    /// The loop is running (or stopped): no card.
    #[must_use]
    pub fn running() -> Self {
        Self {
            paused: false,
            why: "",
            headline: String::new(),
            detail: String::new(),
            unblocks: Vec::new(),
            by: String::new(),
            at: String::new(),
            since_secs: 0,
            others: Vec::new(),
        }
    }

    fn since_line(&self) -> String {
        if self.at.is_empty() {
            // The stamp is in-memory by design, so a restart loses "since".
            "since unknown after restart".to_owned()
        } else if self.since_secs >= 60 {
            format!("for {} min", self.since_secs / 60)
        } else {
            format!("for {} s", self.since_secs)
        }
    }
}

/// Decide why the loop is stopped and what unblocks it. Pure.
#[must_use]
pub fn explain(inputs: &PauseInputs) -> PauseExplanation {
    if inputs.mode != "paused" {
        return PauseExplanation::running();
    }
    let mut applies: Vec<(PauseWhy, PauseExplanation)> = Vec::new();
    if let Some(e) = breaker_card(inputs) {
        applies.push((PauseWhy::EngineBreaker, e));
    }
    if let Some(e) = budget_card(inputs) {
        applies.push((PauseWhy::BudgetCap, e));
    }
    if inputs.quota {
        applies.push((PauseWhy::EngineQuota, quota_card()));
    }
    if let Some(e) = stall_card(inputs) {
        applies.push((PauseWhy::Stall, e));
    }
    if let Some(e) = operator_card(inputs) {
        applies.push((PauseWhy::Operator, e));
    }
    let Some((why, mut lead)) = applies.first().cloned() else {
        // Paused with nothing recorded: say exactly that, never invent a cause.
        return PauseExplanation {
            paused: true,
            why: PauseWhy::Operator.as_str(),
            headline: "Paused — no reason recorded".to_owned(),
            detail: "The loop is stopped and this hub has no recorded cause for it.".to_owned(),
            unblocks: vec![
                "Resume the loop".to_owned(),
                "If it re-pauses, check Settings → budget cap and engine.fallbacks".to_owned(),
            ],
            by: String::new(),
            at: String::new(),
            since_secs: inputs.paused_since_secs.unwrap_or(0),
            others: Vec::new(),
        };
    };
    lead.why = why.as_str();
    lead.others = applies.iter().skip(1).map(|(w, _)| w.as_str()).collect();
    lead
}

fn breaker_card(inputs: &PauseInputs) -> Option<PauseExplanation> {
    let f = inputs.breaker.as_ref()?;
    let auth_death = is_auth_failure(&f.reason);
    let mut unblocks = if auth_death {
        vec![format!(
            "Re-authenticate the {} engine, then resume the loop",
            f.engine
        )]
    } else {
        vec!["Wait for the breaker to reset (a successful run clears it)".to_owned()]
    };
    unblocks.push("Resume the loop once the engine answers again".to_owned());
    Some(PauseExplanation {
        paused: true,
        why: PauseWhy::EngineBreaker.as_str(),
        headline: format!("Engine circuit breaker tripped — {}", f.engine),
        detail: if auth_death {
            format!("{} (credential failure, open since {})", f.reason, f.since)
        } else {
            format!("{} (open since {})", f.reason, f.since)
        },
        unblocks,
        by: "the loop itself".to_owned(),
        at: f.since.clone(),
        since_secs: inputs.paused_since_secs.unwrap_or(0),
        others: Vec::new(),
    })
}

fn budget_card(inputs: &PauseInputs) -> Option<PauseExplanation> {
    let b = inputs.budget?;
    if b.cap_usd <= 0.0 || b.spent_usd < b.cap_usd {
        return None;
    }
    Some(PauseExplanation {
        paused: true,
        why: PauseWhy::BudgetCap.as_str(),
        headline: "Daily spend cap reached".to_owned(),
        detail: format!(
            "${:.2} spent of the ${:.2} daily cap",
            b.spent_usd, b.cap_usd
        ),
        unblocks: vec![
            "Raise the daily budget in Settings → budget cap".to_owned(),
            "Resume the loop after the cap is raised".to_owned(),
        ],
        by: "the loop itself".to_owned(),
        at: String::new(),
        since_secs: inputs.paused_since_secs.unwrap_or(0),
        others: Vec::new(),
    })
}

fn quota_card() -> PauseExplanation {
    PauseExplanation {
        paused: true,
        why: PauseWhy::EngineQuota.as_str(),
        headline: "Every engine is out of quota".to_owned(),
        detail: "The loop paused itself — no configured engine can take a call.".to_owned(),
        unblocks: vec![
            "Pick a fallback engine in Settings → engine.fallbacks".to_owned(),
            "Resume the loop once a fallback is configured".to_owned(),
        ],
        by: "the loop itself".to_owned(),
        at: String::new(),
        since_secs: 0,
        others: Vec::new(),
    }
}

fn stall_card(inputs: &PauseInputs) -> Option<PauseExplanation> {
    let s = inputs.stall.as_ref()?;
    Some(PauseExplanation {
        paused: true,
        why: PauseWhy::Stall.as_str(),
        headline: "Stalled — no cycle event since the watchdog horizon".to_owned(),
        detail: format!("Last cycle event: {} at {}", s.role, s.at),
        unblocks: vec![
            format!("Restore the stalled role ({})", s.role),
            "Resume the loop once the role is alive again".to_owned(),
        ],
        by: "the loop itself".to_owned(),
        at: s.at.clone(),
        since_secs: inputs.paused_since_secs.unwrap_or(0),
        others: Vec::new(),
    })
}

fn operator_card(inputs: &PauseInputs) -> Option<PauseExplanation> {
    let o = inputs.operator.as_ref()?;
    let by = if o.by.is_empty() {
        "an operator".to_owned()
    } else {
        o.by.clone()
    };
    let detail = if o.reason.is_empty() {
        // The honest empty state: never fabricate a reason for a human action.
        format!("Paused from the dashboard by {by} — no reason recorded")
    } else {
        format!("Paused from the dashboard by {by} — “{}”", o.reason)
    };
    Some(PauseExplanation {
        paused: true,
        why: PauseWhy::Operator.as_str(),
        headline: format!("Paused by {by}"),
        detail,
        unblocks: vec!["Resume the loop".to_owned()],
        by,
        at: o.at.clone(),
        since_secs: inputs.paused_since_secs.unwrap_or(0),
        others: Vec::new(),
    })
}

/// Whether a failure line is a credential death rather than a transient infra
/// fault — the same wording `runner.rs` keys its auth-death detection on.
fn is_auth_failure(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    r.contains("auth") || r.contains("credential") || r.contains("oauth") || r.contains("401")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paused() -> PauseInputs {
        PauseInputs {
            mode: "paused".to_owned(),
            paused_since_secs: Some(120),
            ..PauseInputs::default()
        }
    }

    #[test]
    fn a_running_loop_renders_the_zero_state_and_no_card() {
        let e = explain(&PauseInputs {
            mode: "running".to_owned(),
            ..PauseInputs::default()
        });
        assert!(!e.paused);
        assert_eq!(e.why, "");
        assert!(e.unblocks.is_empty());
    }

    #[test]
    fn a_stopped_loop_is_not_a_pause() {
        let e = explain(&PauseInputs {
            mode: "stopped".to_owned(),
            ..PauseInputs::default()
        });
        assert!(
            !e.paused,
            "stopped is a deliberate end, not a pause to explain"
        );
    }

    #[test]
    fn the_breaker_outranks_budget_quota_stall_and_operator() {
        let mut i = paused();
        i.breaker = Some(BreakerFault {
            engine: "claude".to_owned(),
            reason: "Failed to authenticate: OAuth expired".to_owned(),
            since: "2026-09-13T10:00:00Z".to_owned(),
        });
        i.budget = Some(BudgetBreach {
            spent_usd: 12.0,
            cap_usd: 10.0,
        });
        i.quota = true;
        i.stall = Some(StallEvidence {
            role: "DEV-BUG".to_owned(),
            at: "2026-09-13T09:40:00Z".to_owned(),
        });
        i.operator = Some(OperatorPause {
            by: "ana".to_owned(),
            at: "2026-09-13T09:00:00Z".to_owned(),
            reason: "reviewing".to_owned(),
        });
        let e = explain(&i);
        assert_eq!(e.why, "engine_breaker");
        assert_eq!(
            e.others,
            vec!["budget_cap", "engine_quota", "stall", "operator"],
            "the card lists the rest, most severe first"
        );
    }

    #[test]
    fn budget_outranks_quota_stall_and_operator() {
        let mut i = paused();
        i.budget = Some(BudgetBreach {
            spent_usd: 10.5,
            cap_usd: 10.0,
        });
        i.quota = true;
        i.operator = Some(OperatorPause::default());
        let e = explain(&i);
        assert_eq!(e.why, "budget_cap");
        assert!(e.headline.contains("Daily spend cap"));
        assert!(
            e.detail.contains("$10.50") && e.detail.contains("$10.00"),
            "the cap amount is named: {}",
            e.detail
        );
        assert!(e.unblocks.iter().any(|u| u.contains("budget cap")));
    }

    #[test]
    fn a_budget_below_the_cap_does_not_claim_a_breach() {
        let mut i = paused();
        i.budget = Some(BudgetBreach {
            spent_usd: 3.0,
            cap_usd: 10.0,
        });
        let e = explain(&i);
        assert_ne!(e.why, "budget_cap", "under the cap is not a breach");
    }

    #[test]
    fn a_quota_pause_names_the_fallback_engine_fix() {
        let mut i = paused();
        i.quota = true;
        let e = explain(&i);
        assert_eq!(e.why, "engine_quota");
        assert!(e.unblocks.iter().any(|u| u.contains("engine.fallbacks")));
    }

    #[test]
    fn a_stall_names_the_last_cycle_event_rather_than_a_bare_stalled() {
        let mut i = paused();
        i.stall = Some(StallEvidence {
            role: "DEV-BUG".to_owned(),
            at: "2026-09-13T09:40:00Z".to_owned(),
        });
        let e = explain(&i);
        assert_eq!(e.why, "stall");
        assert!(
            e.detail.contains("DEV-BUG") && e.detail.contains("2026-09-13T09:40:00Z"),
            "role + timestamp are the evidence: {}",
            e.detail
        );
        assert!(e.unblocks.iter().any(|u| u.contains("DEV-BUG")));
    }

    #[test]
    fn an_operator_pause_with_no_recorded_reason_says_so_and_never_fabricates_one() {
        let mut i = paused();
        i.operator = Some(OperatorPause::default());
        let e = explain(&i);
        assert_eq!(e.why, "operator");
        assert!(
            e.detail.contains("no reason recorded"),
            "the empty state is honest: {}",
            e.detail
        );
    }

    #[test]
    fn an_operator_pause_carries_who_and_when_and_the_inverse_action() {
        let mut i = paused();
        i.operator = Some(OperatorPause {
            by: "ana".to_owned(),
            at: "2026-09-13T09:00:00Z".to_owned(),
            reason: "reviewing the sprint".to_owned(),
        });
        let e = explain(&i);
        assert_eq!(e.by, "ana");
        assert_eq!(e.at, "2026-09-13T09:00:00Z");
        assert_eq!(e.since_secs, 120);
        assert_eq!(e.unblocks, vec!["Resume the loop".to_owned()]);
    }

    #[test]
    fn a_pause_with_no_recorded_cause_says_unknown_instead_of_guessing() {
        let e = explain(&paused());
        assert!(e.paused);
        assert!(e.headline.contains("no reason recorded"));
        assert!(e.at.is_empty(), "no timestamp is invented");
    }

    #[test]
    fn a_pause_with_no_in_memory_stamp_admits_since_is_unknown_after_a_restart() {
        let mut i = paused();
        i.paused_since_secs = None;
        i.operator = Some(OperatorPause {
            by: "ana".to_owned(),
            at: String::new(),
            reason: String::new(),
        });
        let e = explain(&i);
        assert_eq!(e.since_secs, 0);
        assert!(e.since_line().contains("since unknown after restart"));
    }

    #[test]
    fn a_transient_breaker_fault_does_not_claim_a_credential_fix() {
        let mut i = paused();
        i.breaker = Some(BreakerFault {
            engine: "opencode".to_owned(),
            reason: "connection reset by peer".to_owned(),
            since: "2026-09-13T10:00:00Z".to_owned(),
        });
        let e = explain(&i);
        assert!(
            e.unblocks.iter().any(|u| u.contains("breaker to reset")),
            "a transient fault waits for the reset: {:?}",
            e.unblocks
        );
        assert!(!e.unblocks.iter().any(|u| u.contains("Re-authenticate")));
    }
}
