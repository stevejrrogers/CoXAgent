//! Stale human-gate approval escalation over SLA windows (CXA-F236).
//!
//! When `human.gate_ready` / `human.gate_verify` is on, a ticket holds at
//! `Pending` (with design) or `Fixed` (with evidence) until a PERSON acts —
//! and nothing prompts anyone when that person is away. This module ladders
//! each stale gate hold up a config-driven chain of escalation tiers: at each
//! tier the hold is surfaced to a wider set of roles, and the final tier
//! raises an SM impediment instead of laddering further.
//!
//! The gate invariants are untouched: escalation NEVER issues a transition or
//! any other gate side-effect (see `gate_promises.rs` — every gate edge stays
//! a human decision); it only records hold state, posts notifications, and
//! widens the AUDIENCE of who is asked. Eligible fallback approvers are
//! derived strictly from the `AuthRole` permission predicates, so the ladder
//! can never widen authority past the policy guardrails.
//!
//! Everything decision-shaped is a pure function over the state snapshot;
//! the use-case driver only persists through the state-store port.

use super::RunCycleUseCase;
use crate::auth::AuthRole;
use crate::config::HumanConfig;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use crate::state::{GateHold, ProjectState};
use coxagent_domain::Status;

/// Which human gate a ticket is waiting at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// `Pending → Ready` — a designed ticket waiting for a person's approval.
    Ready,
    /// `Fixed → Verified` — fixed work with evidence waiting for a verdict.
    Verify,
}

impl GateKind {
    /// The `AuthRole` predicate that decides who may take this gate's
    /// decision — the ONLY source of who the escalation may widen to.
    #[must_use]
    pub fn permits(self, role: AuthRole) -> bool {
        match self {
            Self::Ready => role.can_approve_ready(),
            Self::Verify => role.can_verify(),
        }
    }

    /// The roles that already own the gate before any escalation — the inbox's
    /// primary actors ("BA/PO" for approvals, "QA" for verdicts).
    #[must_use]
    pub fn primary_owners(self) -> Vec<AuthRole> {
        match self {
            Self::Ready => vec![AuthRole::Ba, AuthRole::Po],
            Self::Verify => vec![AuthRole::Qa],
        }
    }

    /// Human label of the decision being waited on.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "approval",
            Self::Verify => "verification",
        }
    }

    /// Config name of the gate, for messages.
    #[must_use]
    pub fn gate_name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Verify => "verify",
        }
    }
}

/// One rung of the ladder crossed this pass for one stale hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierAdvance {
    /// 1-based tier index reached.
    pub tier: u32,
    /// Roles newly added to the audience at this tier (ordered).
    pub added: Vec<AuthRole>,
    /// The full ordered eligible fallback approver list at this tier.
    pub audience: Vec<AuthRole>,
    /// True for the last configured tier — its action is the SM impediment
    /// digest, not another ladder rung.
    pub final_tier: bool,
}

/// One stale gate hold and the tiers it crossed in this pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateEscalation {
    pub ticket: String,
    pub kind: GateKind,
    /// Minutes the ticket has waited at the gate.
    pub age_minutes: u64,
    pub advances: Vec<TierAdvance>,
}

/// What one escalation pass decided, as data — the use case only applies it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateEscalationPlan {
    /// Tickets currently waiting at a gate (a decision resets the ladder, so
    /// holds for anything else are pruned).
    pub waiting_ids: Vec<String>,
    /// Holds to write: first-observation timestamps and reached tiers.
    pub upsert_holds: Vec<(String, GateHold)>,
    /// Stale holds that crossed at least one tier boundary this pass.
    pub escalations: Vec<GateEscalation>,
}

/// The tickets waiting at a human gate — exactly the items the hybrid inbox
/// surfaces as approve/verify actions (server/inbox.rs), so escalation and
/// inbox can never disagree about what is waiting.
#[must_use]
pub fn waiting_gate_items(state: &ProjectState, human: &HumanConfig) -> Vec<(String, GateKind)> {
    state
        .tickets
        .iter()
        .filter_map(|t| {
            let id = t.id().to_string();
            if human.gate_ready
                && t.status() == Status::Pending
                && t.design().technical.is_some()
            {
                return Some((id, GateKind::Ready));
            }
            if human.gate_verify
                && t.status() == Status::Fixed
                && state.ticket_evidence.contains_key(&id)
            {
                return Some((id, GateKind::Verify));
            }
            None
        })
        .collect()
}

/// Ordered eligible fallback approvers for a hold escalated to `reached`
/// tiers: the gate's primary owner roles first, then each configured tier's
/// roles in config order, duplicates dropped. A configured role whose
/// `AuthRole` predicate does not permit it to take the decision is dropped —
/// the ladder never widens authority past the policy guardrails (and an
/// unknown label parses to `Viewer`, which fails both predicates, so it is
/// dropped too).
#[must_use]
pub fn ordered_fallback_approvers(
    kind: GateKind,
    tiers: &[crate::config::GateEscalationTier],
    reached: u32,
) -> Vec<AuthRole> {
    let owners = kind.primary_owners();
    let mut out = owners.clone();
    for tier in tiers.iter().take(reached as usize) {
        for label in &tier.roles {
            let role = AuthRole::from_str_lenient(label);
            if kind.permits(role) && !out.contains(&role) {
                out.push(role);
            }
        }
    }
    // Terminal target (the "running role absent" edge): a ladder whose every
    // configured role fails the gate's predicate widens nothing — keep the
    // highest-permitting role as the terminal target rather than deadlocking
    // on an audience that cannot act.
    if reached > 0 && out.len() == owners.len() {
        if let Some(top) = AuthRole::all().iter().copied().find(|r| kind.permits(*r)) {
            if !out.contains(&top) {
                out.push(top);
            }
        }
    }
    out
}

/// The gate-wait's start time from the activity journal: the entry recorded
/// when the ticket entered the status it is waiting in. `None` when the
/// journal has rotated past it (the feed is bounded) — the caller then starts
/// measuring from first observation instead of fabricating an age.
fn derived_entry_unix_s(state: &ProjectState, id: &str, kind: GateKind) -> Option<i64> {
    state
        .activity
        .iter()
        .rev()
        .find(|e| {
            e.ticket.as_deref() == Some(id)
                && match kind {
                    GateKind::Ready => {
                        (e.agent == "SA" && e.action == "designed (technical)")
                            || (e.agent == "PD" && e.action == "designed UX & readied")
                    }
                    GateKind::Verify => {
                        (e.agent == "DEV-BUG" && e.action == "fixed bug")
                            || (e.agent == "DEV-FEATURE" && e.action == "implemented feature")
                    }
                }
        })
        .and_then(|e| rfc3339_to_unix_s(&e.at))
}

fn rfc3339_to_unix_s(at: &str) -> Option<i64> {
    time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(time::OffsetDateTime::unix_timestamp)
}

/// Unix seconds now — the clock every age in this module is measured on.
#[must_use]
pub fn now_unix_s() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The one decision pass (pure): given the snapshot, which waiting-human gate
/// holds have exceeded their configured SLA window, which tier boundaries they
/// crossed, and the ordered eligible fallback approvers each crossing widens
/// to. Gate holds waiting under the SLA, and items with no derivable entry
/// time (clock starts at first observation), plan nothing.
#[must_use]
pub fn plan_gate_escalations(
    state: &ProjectState,
    human: &HumanConfig,
    now_unix_s: i64,
) -> GateEscalationPlan {
    let tiers = &human.gate_escalation_tiers;
    // Documented defaults (SLA 0, no tiers) mean disabled — exact current
    // behaviour, no timeout, no notifications.
    if human.gate_sla_minutes == 0 || tiers.is_empty() {
        return GateEscalationPlan::default();
    }
    let waiting = waiting_gate_items(state, human);
    let mut plan = GateEscalationPlan {
        waiting_ids: waiting.iter().map(|(id, _)| id.clone()).collect(),
        ..GateEscalationPlan::default()
    };
    for (id, kind) in waiting {
        // A persisted hold counts as THIS wait only if it is waiting at the
        // same gate: approval followed by the work later reaching the verify
        // gate is a NEW wait — it must not inherit the old clock or tier.
        let prior = state
            .gate_holds
            .get(&id)
            .copied()
            .filter(|h| h.at_ready_gate == (kind == GateKind::Ready));
        let entry = prior
            .filter(|h| h.entered_at_unix_s > 0)
            .map(|h| h.entered_at_unix_s)
            .or_else(|| derived_entry_unix_s(state, &id, kind));
        let Some(entry) = entry else {
            // First observation, no derivable status entry: start the clock
            // now. Never fabricate an age from nothing — and never escalate
            // on an age we invented.
            plan.upsert_holds.push((
                id,
                GateHold {
                    entered_at_unix_s: now_unix_s,
                    escalated_to_tier: 0,
                    at_ready_gate: kind == GateKind::Ready,
                },
            ));
            continue;
        };
        let current_tier = prior.map_or(0, |h| h.escalated_to_tier);
        let age_minutes = u64::try_from((now_unix_s - entry).max(0)).unwrap_or(0) / 60;
        // Persist the entry time so the bounded activity journal rotating
        // away can never reset a measured wait.
        plan.upsert_holds.push((
            id.clone(),
            GateHold {
                entered_at_unix_s: entry,
                escalated_to_tier: current_tier,
                at_ready_gate: kind == GateKind::Ready,
            },
        ));
        if age_minutes < human.gate_sla_minutes {
            continue;
        }
        // Tiers are listed earliest-first; the target is the highest tier
        // whose window (minutes since gate entry) has elapsed. A tier only
        // ever advances — reaching a lower window than a persisted tier
        // (config change) demotes nothing.
        let target_tier = u32::try_from(
            tiers
                .iter()
                .filter(|t| t.after_minutes <= age_minutes)
                .count()
                .max(current_tier as usize),
        )
        .unwrap_or(current_tier);
        if target_tier <= current_tier {
            continue;
        }
        let mut advances = Vec::new();
        for tier in (current_tier + 1)..=target_tier {
            let audience = ordered_fallback_approvers(kind, tiers, tier);
            let previous = ordered_fallback_approvers(kind, tiers, tier - 1);
            let added: Vec<AuthRole> = audience
                .iter()
                .filter(|r| !previous.contains(r))
                .copied()
                .collect();
            advances.push(TierAdvance {
                tier,
                added,
                audience,
                final_tier: tier as usize == tiers.len(),
            });
        }
        plan.escalations.push(GateEscalation {
            ticket: id.clone(),
            kind,
            age_minutes,
            advances,
        });
        if let Some((_, hold)) = plan.upsert_holds.iter_mut().find(|(h, _)| *h == id) {
            hold.escalated_to_tier = target_tier;
        }
    }
    plan
}

/// Role labels for a message: the newly-widened roles, or — when a tier
/// widens nothing (guardrail-filtered) — the full audience it keeps waiting.
fn audience_names(adv: &TierAdvance) -> String {
    let roles = if adv.added.is_empty() {
        &adv.audience
    } else {
        &adv.added
    };
    if roles.is_empty() {
        return "the gate owner".to_owned();
    }
    roles
        .iter()
        .map(|r| r.label())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Apply the plan: persist holds, prune decided ones, and post the
/// escalation notifications. Notification-ONLY by construction — no ticket
/// mutation, no comment, no transition (the gate decisions stay human).
fn apply_gate_escalation_plan(
    state: &mut ProjectState,
    plan: &GateEscalationPlan,
    sla_minutes: u64,
) {
    state
        .gate_holds
        .retain(|id, _| plan.waiting_ids.iter().any(|w| w == id));
    for (id, hold) in &plan.upsert_holds {
        state.gate_holds.insert(id.clone(), *hold);
    }
    for esc in &plan.escalations {
        for adv in &esc.advances {
            let id = &esc.ticket;
            let names = audience_names(adv);
            let msg = if adv.final_tier {
                format!(
                    "🧯 {id} {} has waited {}m at the {} gate (SLA {sla_minutes}m) — FINAL \
                     escalation tier {}: SM impediment — a human gate is the throughput \
                     bottleneck. Approve, verify, reject, or reassign ({names}).",
                    esc.kind.label(),
                    esc.age_minutes,
                    esc.kind.gate_name(),
                    adv.tier,
                )
            } else {
                format!(
                    "⏰ {id} {} has waited {}m at the {} gate (SLA {sla_minutes}m) — escalation \
                     tier {}: now also waiting on {names}.",
                    esc.kind.label(),
                    esc.age_minutes,
                    esc.kind.gate_name(),
                    adv.tier,
                )
            };
            state.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            state.log_activity(
                "SM",
                &format!(
                    "escalated a stale {} gate hold to tier {}",
                    esc.kind.gate_name(),
                    adv.tier
                ),
                Some(id.clone()),
            );
        }
    }
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// CXA-F236: escalate stale human-gate holds up the configured tier
    /// ladder. Runs beside the question-SLA escalation in every cycle. With
    /// the documented defaults (SLA 0 / no tiers) this is a no-op — an
    /// unconfigured project behaves exactly as before.
    pub(super) async fn escalate_stale_gate_holds(&self) {
        let human = self.config.workflow.human.clone();
        if human.gate_sla_minutes == 0 || human.gate_escalation_tiers.is_empty() {
            return;
        }
        let now = now_unix_s();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            let plan = plan_gate_escalations(s, &human, now);
            apply_gate_escalation_plan(s, &plan, human.gate_sla_minutes);
            Ok(())
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_defaults_plan_nothing() {
        let state = ProjectState::default();
        let human = HumanConfig::default();
        assert!(plan_gate_escalations(&state, &human, 1_000).escalations.is_empty());
    }

    #[test]
    fn unknown_tier_role_label_is_dropped_not_deadlocked() {
        let tiers = vec![crate::config::GateEscalationTier {
            after_minutes: 30,
            roles: vec!["nonexistent-role".to_owned()],
        }];
        let approvers = ordered_fallback_approvers(GateKind::Ready, &tiers, 1);
        // Guardrail filter empties the widening; the highest-permitting role
        // becomes the terminal target instead of an audience that cannot act.
        assert!(approvers.contains(&AuthRole::Admin));
        assert!(!approvers.contains(&AuthRole::Viewer));
    }
}
