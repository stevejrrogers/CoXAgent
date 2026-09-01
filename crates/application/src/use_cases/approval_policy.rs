//! What the adaptive approval gate learned, made readable for the humans who
//! taught it (CXA-F303).
//!
//! Pure over `&ProjectState` + the adaptive config: the caller (one thin
//! endpoint) reads state and the wall clock, every row and verdict falls out
//! here through the SAME learner and risk function the cycle pass uses
//! ([`rule_for`], [`assess`], [`gate_allows`]) — the panel can never drift
//! from what the loop actually does. See docs/ADAPTIVE_APPROVAL.md.

use serde::Serialize;

use super::approval_memory::{rule_for, Rule};
use super::approval_risk::{assess, shape_key, Lane};
use crate::config::AdaptiveConfig;
use crate::state::ProjectState;
use coxagent_domain::Status;

/// The one allow/deny decision the adaptive pass makes per candidate
/// (`cycle/audits.rs`): the risk lane and the learned rule vote, and a
/// preflight-fix rule is satisfied once the reason it was learned for — the
/// acceptance criteria — is addressed on the ticket itself. Shared by the
/// pass and the policy read model so the panel always describes exactly what
/// the loop will do.
#[must_use]
pub fn gate_allows(lane: Lane, learned: &Rule, has_acceptance_criteria: bool) -> bool {
    match (lane, learned) {
        // Risk says routine; or risk says ask but this team approves the
        // shape every time — trust the humans over the heuristic.
        (Lane::Auto, Rule::KeepAsking | Rule::AutoApprove { .. })
        | (Lane::Ask, Rule::AutoApprove { .. }) => true,
        (Lane::Auto, Rule::PreflightFix { .. }) => has_acceptance_criteria,
        _ => false,
    }
}

/// Decision counts behind one shape, straight from the recorded samples.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ShapeSamples {
    pub approve: usize,
    pub reject: usize,
    pub undo: usize,
}

/// One ticket shape the adaptive gate knows, with the rule it currently
/// learned, the human decisions behind it, and any operator override.
#[derive(Debug, Clone, Serialize)]
pub struct ShapePolicy {
    pub shape: String,
    /// What the learner concluded from the recorded samples.
    pub learned: Rule,
    /// A human forced this shape back to always-ask (`ask_again_shapes`) —
    /// outranks anything learned.
    pub overridden: bool,
    /// `"ask"` when overridden or the learned rule is not auto-approve,
    /// `"auto"` otherwise — what the pass actually does for this shape.
    pub effective: &'static str,
    /// Approve/reject/undo counts behind [`ShapePolicy::learned`].
    pub samples: ShapeSamples,
    /// Everyone who decided a sample on this shape.
    pub deciders: Vec<String>,
    /// RFC3339 time of the most recent recorded decision, if any.
    pub last_decision_at: Option<String>,
    /// The reason string recorded with the last decision (rejects carry the
    /// useful ones).
    pub last_reason: Option<String>,
}

/// A pending designed ticket the gate is currently deciding alone — the
/// "which tickets is the loop deciding alone today, and why?" row (AC3).
#[derive(Debug, Clone, Serialize)]
pub struct GateDecision {
    pub ticket: String,
    pub title: String,
    pub shape: String,
    /// Whether the next cycle pass would auto-approve it as-is.
    pub will_auto: bool,
    /// What drives that: `"learned"` (recorded human approvals), `"preflight"`
    /// (a rejection reason now addressed), `"risk"` (the risk heuristic), or
    /// `"override"` (a human forced the shape to always-ask).
    pub driver: &'static str,
    /// 0–100 risk score from the announcement.
    pub risk_score: u8,
    /// The announcement's why string.
    pub risk_why: String,
    /// True when the shape is in `ask_again_shapes` — the override, not the
    /// rule or the heuristic, held it.
    pub blocked_by_override: bool,
}

/// An auto-approval still inside its undo window: the panel lists it with the
/// remaining time and wires Undo to the existing per-ticket endpoint (AC4).
#[derive(Debug, Clone, Serialize)]
pub struct UndoableApproval {
    pub ticket: String,
    pub title: String,
    pub shape: String,
    pub minutes_left: u64,
}

/// An auto-approval whose undo window closed: history, attributed per shape —
/// it stays inspectable instead of vanishing with its map entry (AC4).
#[derive(Debug, Clone, Serialize)]
pub struct ExpiredApproval {
    pub ticket: String,
    pub title: String,
    pub shape: String,
    /// RFC3339 time the undo window opened.
    pub approved_at: String,
    /// Minutes elapsed since it was auto-approved; `None` when the stamp
    /// cannot be parsed (shown as unknown age, never guessed).
    pub minutes_ago: Option<u64>,
}

/// The whole policy view: one GET of `/api/projects/:pid/approval-policy`.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyOverview {
    /// `workflow.human.adaptive.enabled` in project config.
    pub enabled: bool,
    /// The other half of the pass's early return: the ready gate must be on.
    pub gate_ready: bool,
    /// Minutes an auto-approval can be pulled back.
    pub undo_window_minutes: u64,
    /// Consistent decisions before a shape shifts to auto.
    pub learn_after_samples: usize,
    /// Auto-approvals per cycle cap.
    pub max_auto_per_cycle: usize,
    /// True when the pass will not run at all (`enabled` or `gate_ready`
    /// false) — the panel renders the explicit gate-off state from this and
    /// lists no applicable rules (AC5).
    pub gate_off: bool,
    /// Every known shape with its rule — empty when [`PolicyOverview::gate_off`].
    pub shapes: Vec<ShapePolicy>,
    /// Pending designed tickets the gate is deciding this cycle (AC3).
    pub deciding: Vec<GateDecision>,
    /// Auto-approvals still undoable, with remaining minutes (AC4).
    pub undoable: Vec<UndoableApproval>,
    /// Expired auto-approvals as history, attributed per shape (AC4).
    pub expired: Vec<ExpiredApproval>,
}

/// Minutes elapsed between two RFC3339 stamps, or `None` when either stamp
/// is unparseable or lies in the future (negative elapsed) — the caller then
/// treats the age as unknown, never guesses a number.
fn minutes_between(from_rfc3339: &str, now_rfc3339: &str) -> Option<u64> {
    let parse =
        |s: &str| time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339);
    let secs = (parse(now_rfc3339).ok()? - parse(from_rfc3339).ok()?).whole_seconds();
    u64::try_from(secs).ok().map(|s| s / 60)
}

/// A ticket shape is "ask" when a human overrode it or the learned rule is
/// not auto-approve; only an unoverridden `AutoApprove` keeps deciding alone.
fn effective_rule(overridden: bool, learned: &Rule) -> &'static str {
    if overridden || !matches!(learned, Rule::AutoApprove { .. }) {
        "ask"
    } else {
        "auto"
    }
}

/// One row per shape the gate knows, aggregated from the recorded samples
/// and the operator's overrides, sorted overridden-first then by name.
fn shape_rows(
    state: &ProjectState,
    overrides: &std::collections::BTreeSet<&str>,
    learn_after: usize,
) -> Vec<ShapePolicy> {
    // Every shape the gate knows: recorded decisions, the operator's
    // overrides, and the board's tickets via shape_key — an override on a
    // shape with zero samples must still show as overridden/ask.
    let mut known: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for s in &state.approval_samples {
        known.insert(s.shape.clone());
    }
    for s in &state.ask_again_shapes {
        known.insert(s.clone());
    }
    for t in &state.tickets {
        known.insert(shape_key(t));
    }

    let mut shapes: Vec<ShapePolicy> = known
        .into_iter()
        .map(|shape| {
            let mine = || state.approval_samples.iter().filter(|s| s.shape == shape);
            let samples = ShapeSamples {
                approve: mine().filter(|s| s.decision == "approve").count(),
                reject: mine().filter(|s| s.decision == "reject").count(),
                undo: mine().filter(|s| s.decision == "undo").count(),
            };
            // Samples are appended chronologically, so the last entry for the
            // shape IS the most recent decision.
            let last = mine().next_back();
            let deciders: Vec<String> = {
                let mut d: std::collections::BTreeSet<String> =
                    mine().map(|s| s.by.clone()).collect();
                d.retain(|decider| !decider.trim().is_empty());
                d.into_iter().collect()
            };
            let learned = rule_for(&shape, &state.approval_samples, learn_after);
            let overridden = overrides.contains(shape.as_str());
            ShapePolicy {
                effective: effective_rule(overridden, &learned),
                learned,
                overridden,
                samples,
                deciders,
                last_decision_at: last.map(|s| s.at.clone()),
                last_reason: last
                    .map(|s| s.reason.clone())
                    .filter(|r| !r.trim().is_empty()),
                shape,
            }
        })
        .collect();
    // Overridden shapes first — the operator's own interventions lead the
    // list — then alphabetically for determinism.
    shapes.sort_by(|a, b| {
        b.overridden
            .cmp(&a.overridden)
            .then_with(|| a.shape.cmp(&b.shape))
    });
    shapes
}

/// The pending designed tickets the pass would evaluate, judged through the
/// SAME shared decision function (`gate_allows`) — the panel can never drift
/// from what the loop actually does.
fn deciding_rows(
    state: &ProjectState,
    overrides: &std::collections::BTreeSet<&str>,
    learn_after: usize,
) -> Vec<GateDecision> {
    // Prior art per shape, exactly as the pass counts it.
    let mut shipped: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for t in &state.tickets {
        if matches!(
            t.status(),
            Status::Done | Status::Documented | Status::Verified
        ) {
            *shipped.entry(shape_key(t)).or_insert(0) += 1;
        }
    }
    let parked: std::collections::BTreeSet<String> =
        state.ticket_fail_attempts.keys().cloned().collect();

    state
        .tickets
        .iter()
        .filter(|t| t.status() == Status::Pending && t.design().technical.is_some())
        .map(|t| {
            let shape = shape_key(t);
            let verdict = assess(
                t,
                shipped.get(&shape).copied().unwrap_or(0),
                parked.contains(&t.id().to_string()),
            );
            let learned = rule_for(&shape, &state.approval_samples, learn_after);
            let overridden = overrides.contains(shape.as_str());
            let has_criteria = !t.acceptance_criteria().is_empty();
            let will_auto = !overridden && gate_allows(verdict.lane, &learned, has_criteria);
            let driver = if overridden {
                "override"
            } else if matches!(learned, Rule::AutoApprove { .. }) {
                "learned"
            } else if matches!(learned, Rule::PreflightFix { .. }) && will_auto {
                "preflight"
            } else {
                "risk"
            };
            GateDecision {
                ticket: t.id().to_string(),
                title: t.title().to_owned(),
                shape,
                will_auto,
                driver,
                risk_score: verdict.score,
                risk_why: verdict.why,
                blocked_by_override: overridden,
            }
        })
        .collect()
}

/// The undo window split into still-pullable entries (with remaining
/// minutes) and history past the window, both attributed per shape. An
/// entry only leaves `auto_approved_at` on undo/approval, so closed windows
/// stay inspectable here.
fn undo_lists(
    state: &ProjectState,
    undo_window: u64,
    now: &str,
) -> (Vec<UndoableApproval>, Vec<ExpiredApproval>) {
    let mut undoable = Vec::new();
    let mut expired = Vec::new();
    for (id, at) in &state.auto_approved_at {
        let Some(t) = state.tickets.iter().find(|t| t.id().as_str() == id) else {
            continue; // dangling entry — integrity heals it at load
        };
        let age_min = minutes_between(at, now);
        let row_shape = shape_key(t);
        match age_min {
            Some(age) if age <= undo_window => undoable.push(UndoableApproval {
                ticket: id.clone(),
                title: t.title().to_owned(),
                shape: row_shape,
                minutes_left: undo_window - age,
            }),
            _ => expired.push(ExpiredApproval {
                ticket: id.clone(),
                title: t.title().to_owned(),
                shape: row_shape,
                approved_at: at.clone(),
                minutes_ago: age_min,
            }),
        }
    }
    (undoable, expired)
}

/// The policy read model over freshly loaded state (CXA-F303). `now` is
/// passed in — RFC3339 — so the whole thing stays a pure function and the
/// undo windows are testable with fixed stamps.
#[must_use]
pub fn policy_overview(
    state: &ProjectState,
    adaptive: &AdaptiveConfig,
    gate_ready: bool,
    now: &str,
) -> PolicyOverview {
    let undo_window = adaptive.undo_window_minutes();
    let learn_after = adaptive.learn_after_samples();
    let gate_off = !adaptive.enabled || !gate_ready;
    let overview = PolicyOverview {
        enabled: adaptive.enabled,
        gate_ready,
        undo_window_minutes: undo_window,
        learn_after_samples: learn_after,
        max_auto_per_cycle: adaptive.max_auto_per_cycle(),
        gate_off,
        shapes: Vec::new(),
        deciding: Vec::new(),
        undoable: Vec::new(),
        expired: Vec::new(),
    };
    if gate_off {
        return overview; // AC5: off means off — no applicable rules listed.
    }
    let overrides: std::collections::BTreeSet<&str> =
        state.ask_again_shapes.iter().map(String::as_str).collect();
    let (undoable, expired) = undo_lists(state, undo_window, now);
    PolicyOverview {
        shapes: shape_rows(state, &overrides, learn_after),
        deciding: deciding_rows(state, &overrides, learn_after),
        undoable,
        expired,
        ..overview
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::approval_memory::ApprovalSample;
    use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};

    fn sample(shape: &str, decision: &str, by: &str, reason: &str) -> ApprovalSample {
        ApprovalSample {
            shape: shape.to_owned(),
            decision: decision.to_owned(),
            by: by.to_owned(),
            reason: reason.to_owned(),
            at: "2026-08-31T09:00:00Z".to_owned(),
        }
    }

    fn pending_designed(id: &str, title: &str, kind: TicketType, cx: Complexity) -> Ticket {
        use coxagent_domain::{Role, TechnicalDesign};
        let mut t = Ticket::new(
            TicketId::new(id).expect("valid id"),
            kind,
            title.to_owned(),
            "body".to_owned(),
            Priority::Medium,
            cx,
            false,
        )
        .expect("valid ticket");
        t.set_technical_design(
            Role::Sa,
            TechnicalDesign {
                files: vec!["crates/app/tests/policy_tests.rs".to_owned()],
                ..TechnicalDesign::default()
            },
        )
        .expect("SA owns the design");
        t
    }

    fn gate() -> AdaptiveConfig {
        AdaptiveConfig {
            enabled: true,
            ..AdaptiveConfig::default()
        }
    }

    const NOW: &str = "2026-08-31T12:00:00Z";

    #[test]
    fn aggregates_counts_and_deciders_without_cross_shape_leakage() {
        let mut s = ProjectState::default();
        for _ in 0..8 {
            s.approval_samples
                .push(sample("test/small", "approve", "luffy", ""));
        }
        s.approval_samples.push(sample(
            "docs/small",
            "reject",
            "zoro",
            "missing screenshots",
        ));
        let v = policy_overview(&s, &gate(), true, NOW);

        assert_eq!(v.shapes.len(), 2, "both shapes are known");
        let test_row = v.shapes.iter().find(|r| r.shape == "test/small").unwrap();
        assert_eq!(test_row.samples.approve, 8, "no docs rejection leaks in");
        assert_eq!(test_row.samples.reject, 0);
        assert_eq!(test_row.deciders, vec!["luffy".to_owned()]);
        assert!(
            matches!(test_row.learned, Rule::AutoApprove { ref by, samples: 8 } if by == "luffy")
        );
        assert_eq!(test_row.effective, "auto");

        let docs_row = v.shapes.iter().find(|r| r.shape == "docs/small").unwrap();
        assert_eq!(docs_row.samples.reject, 1);
        assert_eq!(docs_row.deciders, vec!["zoro".to_owned()]);
        assert_eq!(
            docs_row.last_reason.as_deref(),
            Some("missing screenshots"),
            "the reason recorded with the last decision"
        );
        assert_eq!(docs_row.effective, "ask", "one rejection keeps asking");
    }

    #[test]
    fn an_override_only_shape_shows_as_overridden_and_asking() {
        let mut s = ProjectState::default();
        s.ask_again_shapes.push("bug/small".to_owned());
        let v = policy_overview(&s, &gate(), true, NOW);

        let row = v.shapes.iter().find(|r| r.shape == "bug/small").unwrap();
        assert!(row.overridden);
        assert_eq!(row.effective, "ask");
        assert_eq!(row.learned, Rule::KeepAsking, "no samples at all");
        assert_eq!(row.samples, ShapeSamples::default());
        assert!(row.deciders.is_empty());
        assert_eq!(row.last_decision_at, None);
    }

    #[test]
    fn an_override_outranks_a_learned_auto_rule() {
        let mut s = ProjectState::default();
        for _ in 0..8 {
            s.approval_samples
                .push(sample("test/small", "approve", "luffy", ""));
        }
        s.ask_again_shapes.push("test/small".to_owned());
        let v = policy_overview(&s, &gate(), true, NOW);

        let row = v.shapes.iter().find(|r| r.shape == "test/small").unwrap();
        assert!(row.overridden, "the audits.rs skip must be honoured");
        assert!(
            matches!(row.learned, Rule::AutoApprove { .. }),
            "the rule is still learned — the override just outranks it"
        );
        assert_eq!(row.effective, "ask");
        assert!(
            v.shapes
                .first()
                .is_some_and(|first| first.shape == "test/small"),
            "overridden shapes sort first"
        );
    }

    #[test]
    fn the_learn_after_threshold_is_honoured() {
        let mut s = ProjectState::default();
        for _ in 0..3 {
            s.approval_samples
                .push(sample("docs/small", "approve", "luffy", ""));
        }
        let v = policy_overview(&s, &gate(), true, NOW);
        let row = v.shapes.iter().find(|r| r.shape == "docs/small").unwrap();
        assert_eq!(row.learned, Rule::KeepAsking, "3 approvals at threshold 8");
        assert_eq!(row.effective, "ask");
    }

    #[test]
    fn deciding_rows_distinguish_learned_rule_from_risk_heuristic() {
        let mut s = ProjectState::default();
        for _ in 0..8 {
            s.approval_samples
                .push(sample("test/small", "approve", "luffy", ""));
        }
        // test/small is Auto by risk AND learned — driven by the learned rule.
        s.tickets.push(pending_designed(
            "CXC-F303-1",
            "Test coverage: policy view",
            TicketType::Chore,
            Complexity::Small,
        ));
        // feature/small gets no samples: if it goes auto at all, the risk
        // heuristic drives it.
        s.tickets.push(pending_designed(
            "CXC-F303-2",
            "Add the policy panel",
            TicketType::Feature,
            Complexity::Small,
        ));
        let v = policy_overview(&s, &gate(), true, NOW);

        let learned_row = v
            .deciding
            .iter()
            .find(|d| d.ticket == "CXC-F303-1")
            .unwrap();
        assert!(learned_row.will_auto);
        assert_eq!(learned_row.driver, "learned");
        assert!(!learned_row.risk_why.is_empty(), "the announcement's why");
        assert!(learned_row.risk_score <= 100, "the announcement's score");

        let risk_row = v
            .deciding
            .iter()
            .find(|d| d.ticket == "CXC-F303-2")
            .unwrap();
        assert_eq!(risk_row.driver, "risk", "no samples: the heuristic alone");
        assert_eq!(risk_row.shape, "feature/small");
    }

    #[test]
    fn an_overridden_shape_never_will_auto_and_names_the_override() {
        let mut s = ProjectState::default();
        for _ in 0..8 {
            s.approval_samples
                .push(sample("test/small", "approve", "luffy", ""));
        }
        s.ask_again_shapes.push("test/small".to_owned());
        s.tickets.push(pending_designed(
            "CXC-F303-1",
            "Test coverage: policy view",
            TicketType::Chore,
            Complexity::Small,
        ));
        let v = policy_overview(&s, &gate(), true, NOW);

        let row = v
            .deciding
            .iter()
            .find(|d| d.ticket == "CXC-F303-1")
            .unwrap();
        assert!(!row.will_auto, "the pass skips overridden shapes");
        assert!(row.blocked_by_override);
        assert_eq!(row.driver, "override");
    }

    #[test]
    fn in_window_auto_approvals_carry_minutes_left_expired_ones_become_history() {
        let mut s = ProjectState::default();
        s.tickets.push(pending_designed(
            "CXC-F303-1",
            "Test coverage: policy view",
            TicketType::Chore,
            Complexity::Small,
        ));
        s.tickets.push(pending_designed(
            "CXC-F303-2",
            "Test coverage: undo listing",
            TicketType::Chore,
            Complexity::Small,
        ));
        // Inside the window: opened one minute ago.
        s.auto_approved_at
            .insert("CXC-F303-1".to_owned(), "2026-08-31T11:59:00Z".to_owned());
        // Long past the 30-minute window.
        s.auto_approved_at
            .insert("CXC-F303-2".to_owned(), "2026-08-30T09:00:00Z".to_owned());
        let v = policy_overview(&s, &gate(), true, NOW);

        assert_eq!(v.undoable.len(), 1);
        let live = &v.undoable[0];
        assert_eq!(live.ticket, "CXC-F303-1");
        assert_eq!(live.shape, "test/small", "attributed via shape_key");
        assert_eq!(live.minutes_left, 29, "window minus age");

        assert_eq!(v.expired.len(), 1);
        let old = &v.expired[0];
        assert_eq!(old.ticket, "CXC-F303-2");
        assert_eq!(
            old.shape, "test/small",
            "history keeps the shape attribution"
        );
        assert_eq!(old.approved_at, "2026-08-30T09:00:00Z");
        assert_eq!(old.minutes_ago, Some(27 * 60));
    }

    /// An unreadable window stamp must land in history with an unknown age —
    /// the caller shows "age unknown", never a guessed number (the same
    /// conservative reading the inbox applies).
    #[test]
    fn an_unreadable_auto_approval_stamp_is_history_with_unknown_age() {
        let mut s = ProjectState::default();
        s.tickets.push(pending_designed(
            "CXC-F303-1",
            "Test coverage: policy view",
            TicketType::Chore,
            Complexity::Small,
        ));
        s.auto_approved_at
            .insert("CXC-F303-1".to_owned(), "not-a-timestamp".to_owned());
        let v = policy_overview(&s, &gate(), true, NOW);

        assert!(
            v.undoable.is_empty(),
            "an unknown age must never count as undoable"
        );
        assert_eq!(v.expired.len(), 1);
        assert_eq!(v.expired[0].shape, "test/small");
        assert_eq!(v.expired[0].minutes_ago, None, "unknown, not zero");
    }

    #[test]
    fn a_disabled_gate_lists_no_applicable_rules() {
        let mut s = ProjectState::default();
        for _ in 0..8 {
            s.approval_samples
                .push(sample("test/small", "approve", "luffy", ""));
        }
        s.ask_again_shapes.push("test/small".to_owned());
        s.auto_approved_at
            .insert("CXC-F303-1".to_owned(), "2026-08-31T11:59:00Z".to_owned());
        let off = AdaptiveConfig {
            enabled: false,
            ..AdaptiveConfig::default()
        };
        let v = policy_overview(&s, &off, true, NOW);
        assert!(v.gate_off);
        assert!(!v.enabled);
        assert!(v.shapes.is_empty(), "no applicable rules while off");
        assert!(v.deciding.is_empty());
        assert!(v.undoable.is_empty());
        assert!(v.expired.is_empty());

        // The ready gate off is the pass's other early return — same honesty.
        let v = policy_overview(&s, &gate(), false, NOW);
        assert!(v.gate_off);
        assert!(v.shapes.is_empty());
    }

    #[test]
    fn the_shared_gate_decision_matches_the_pass_vocabulary() {
        // The exact arms the cycle pass matches on (cycle/audits.rs).
        assert!(gate_allows(Lane::Auto, &Rule::KeepAsking, false));
        assert!(gate_allows(
            Lane::Auto,
            &Rule::AutoApprove {
                by: "luffy".to_owned(),
                samples: 8
            },
            false
        ));
        assert!(gate_allows(
            Lane::Ask,
            &Rule::AutoApprove {
                by: "luffy".to_owned(),
                samples: 8
            },
            false
        ));
        assert!(
            !gate_allows(
                Lane::Auto,
                &Rule::PreflightFix {
                    reason: "no acceptance criteria".to_owned(),
                    samples: 2
                },
                false
            ),
            "a preflight rule holds until the criteria it names exist"
        );
        assert!(gate_allows(
            Lane::Auto,
            &Rule::PreflightFix {
                reason: "no acceptance criteria".to_owned(),
                samples: 2
            },
            true
        ));
        assert!(!gate_allows(Lane::Ask, &Rule::KeepAsking, false));
        assert!(!gate_allows(
            Lane::Ask,
            &Rule::PreflightFix {
                reason: "x".to_owned(),
                samples: 2
            },
            true
        ));
    }
}
