//! The brake cockpit (CXA-F238): operator holds/overrides riding ABOVE the
//! self-tuning loop, plus the read model the dashboard serves. Pure computes
//! over state, re-exported through `crate::metrics` like the burn-down and
//! governance siblings.
//!
//! The law here: `decide_tuning` keeps its hysteresis math untouched — holds
//! compose AFTER the autonomous decision each pass, so an override steers the
//! effective value without ever feeding itself back into the thresholds it
//! disagrees with. Expiry prunes the hold and recomposes, which hands the
//! brake straight back to the loop with no flapping.

use crate::metrics::{agent_evals, compute_burndown, decide_tuning, BURNDOWN_WINDOW_DAYS};
use crate::state::{BrakeHold, ProjectState, Tuning, TuningAuditEntry};
use coxagent_domain::Status;
use serde::Serialize;
use std::collections::BTreeMap;

/// Churn per shipped ticket at or above which the quality brake trips
/// (`decide_tuning`'s `> 1.5` bar, published as data for the cockpit).
pub const CHURN_BRAKE_ON: f64 = 1.5;
/// Churn per shipped ticket below which the quality brake releases
/// (`decide_tuning`'s `< 0.8` bar). In between the brake hysteresis-holds.
pub const CHURN_BRAKE_OFF: f64 = 0.8;
/// Backlog size above which a stalled week trips the intake brake
/// (`decide_tuning`'s `> 25` bar).
pub const BACKLOG_BRAKE_ON: usize = 25;
/// Backlog size below which the intake brake releases (`decide_tuning`'s
/// `< 12` bar).
pub const BACKLOG_BRAKE_OFF: usize = 12;

/// The two brake field names `apply_brake_holds` composes over, card id and
/// `Tuning` field side by side — the ONE table every cockpit surface reads.
const BRAKES: &[(&str, &str)] = &[("quality", "bugs_first"), ("intake", "skip_ba")];

/// Audit entries served per cockpit read — one screenful of the trail; the
/// full bounded history stays in `state.tuning_history`.
const COCKPIT_HISTORY_SHOWN: usize = 50;

/// `true` when `brake` names one of the tunable brake fields.
#[must_use]
pub fn is_brake_field(brake: &str) -> bool {
    BRAKES.iter().any(|(_, field)| *field == brake)
}

/// The brake's value on a tuning struct, by field name.
fn brake_value(tuning: &Tuning, field: &str) -> bool {
    match field {
        "bugs_first" => tuning.bugs_first,
        "skip_ba" => tuning.skip_ba,
        _ => false,
    }
}

/// Write a brake's value on a tuning struct, by field name.
fn set_brake_value(tuning: &mut Tuning, field: &str, value: bool) {
    match field {
        "bugs_first" => tuning.bugs_first = value,
        "skip_ba" => tuning.skip_ba = value,
        _ => {}
    }
}

/// The backlog size the self-tuning pass measures: every ticket still awaiting
/// refinement or work (pending/ready/open). The same count the daily pass
/// feeds `decide_tuning`; one helper so the cockpit and the loop can never
/// drift apart.
#[must_use]
pub fn brake_backlog(state: &ProjectState) -> usize {
    state
        .tickets
        .iter()
        .filter(|t| matches!(t.status(), Status::Pending | Status::Ready | Status::Open))
        .count()
}

/// Parse an RFC3339 stamp; `None` on anything unparsable.
fn parse_rfc3339(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

/// Partition the override map into `(active, expired)` at `now`. A hold with
/// an absent/unparsable bound counts as EXPIRED: the bound is the guarantee
/// that governance cannot be stranded forever, so a corrupt bound fails
/// toward autonomy, not toward the freeze.
#[must_use]
pub fn split_expired_holds(
    holds: &BTreeMap<String, BrakeHold>,
    now: &str,
) -> (
    BTreeMap<String, BrakeHold>,
    BTreeMap<String, BrakeHold>,
) {
    let Some(now_ts) = parse_rfc3339(now) else {
        return (BTreeMap::new(), holds.clone());
    };
    let mut active = BTreeMap::new();
    let mut expired = BTreeMap::new();
    for (brake, hold) in holds {
        match parse_rfc3339(&hold.expires_at) {
            Some(bound) if bound > now_ts => {
                active.insert(brake.clone(), hold.clone());
            }
            _ => {
                expired.insert(brake.clone(), hold.clone());
            }
        }
    }
    (active, expired)
}

/// Compose the effective tuning: the autonomous `raw` decision, with every
/// active operator hold riding on top. A pinned brake (`Some(v)`) is forced
/// to `v` even when the hysteresis disagrees; a frozen brake (`None`) keeps
/// its current value — the loop may not recompute it this pass. Brakes with
/// no hold get the raw policy untouched. Pure.
#[must_use]
pub fn apply_brake_holds(
    raw: &Tuning,
    holds: &BTreeMap<String, BrakeHold>,
    current: &Tuning,
) -> Tuning {
    let mut effective = raw.clone();
    for (_, field) in BRAKES {
        if let Some(hold) = holds.get(*field) {
            let pinned = hold.pinned_value.unwrap_or_else(|| brake_value(current, field));
            set_brake_value(&mut effective, field, pinned);
        }
    }
    effective
}

/// Build one audit entry, stamping the write moment. The one constructor
/// every brake audit goes through (autonomous flips, holds, clears, expiry),
/// so the trail's shape cannot drift between writers.
pub(crate) fn audited_entry(
    actor: &str,
    source: &str,
    brake: &str,
    from: bool,
    to: bool,
    reason: &str,
    until: Option<String>,
) -> TuningAuditEntry {
    TuningAuditEntry {
        at: crate::state::now_rfc3339(),
        actor: actor.to_owned(),
        source: source.to_owned(),
        brake: brake.to_owned(),
        from,
        to,
        reason: reason.to_owned(),
        until,
    }
}

/// Release expired holds and hand those brakes straight back to the loop:
/// prune the map, recompute the autonomous decision over the CURRENT state,
/// and re-apply the still-active holds on top. Every release is audited with
/// source `expiry` (value movement included in `from`/`to`). Idempotent — a
/// second pass over unchanged state changes nothing. Pure. Returns true when
/// any hold expired.
pub fn reconcile_brake_holds(state: &mut ProjectState, now: &str) -> bool {
    let (active, expired) = split_expired_holds(&state.tuning_overrides, now);
    if expired.is_empty() {
        return false;
    }
    let effective = recompose_autonomous(state, &active);
    for (brake, hold) in &expired {
        let entry = audited_entry(
            "SM",
            "expiry",
            brake,
            brake_value(&state.tuning, brake),
            brake_value(&effective, brake),
            "hold expired — autonomous tuning resumed",
            Some(hold.expires_at.clone()),
        );
        state.record_tuning_change(entry);
    }
    state.tuning_overrides = active;
    state.tuning = effective;
    true
}

/// Recompose `state.tuning` from a fresh autonomous decision with `holds`
/// riding on top. `current` for the hysteresis pass is the tuning AS PERSISTED
/// — the same anchoring the daily pass uses, so expiry and clear cannot flip
/// what the bands themselves would hold.
fn recompose_autonomous(
    state: &ProjectState,
    holds: &BTreeMap<String, BrakeHold>,
) -> Tuning {
    let evals = agent_evals(state);
    let backlog = brake_backlog(state);
    let today = crate::state::now_rfc3339()[..10].to_owned();
    let delta = compute_burndown(state, &today, BURNDOWN_WINDOW_DAYS).delta_24h;
    let raw = decide_tuning(&evals, backlog, delta, &state.tuning);
    apply_brake_holds(&raw, holds, &state.tuning)
}

/// Remove one operator hold and recompose immediately (an operator releasing
/// a brake must not wait for the next daily pass). Audits the release with
/// source `clear`. Pure. Returns true when a hold was actually removed.
pub fn clear_brake_hold(state: &mut ProjectState, brake: &str, actor: &str) -> bool {
    let Some(hold) = state.tuning_overrides.remove(brake) else {
        return false;
    };
    let remaining = state.tuning_overrides.clone();
    let effective = recompose_autonomous(state, &remaining);
    let entry = audited_entry(
        actor,
        "clear",
        brake,
        brake_value(&state.tuning, brake),
        brake_value(&effective, brake),
        "hold cleared — autonomous tuning resumed",
        Some(hold.expires_at),
    );
    state.record_tuning_change(entry);
    state.tuning = effective;
    true
}

/// Record a fresh operator hold on `brake` (`bugs_first` / `skip_ba`) and
/// apply it immediately — a pinned value lands now, not on the next daily
/// pass. Audits the intervention with source `hold`, the operator's own
/// reason, and the window. Pure. Returns false for an unknown brake.
pub fn set_brake_hold(state: &mut ProjectState, brake: &str, hold: BrakeHold) -> bool {
    if !is_brake_field(brake) {
        return false;
    }
    let from = brake_value(&state.tuning, brake);
    let to = hold.pinned_value.unwrap_or(from);
    state
        .tuning_overrides
        .insert(brake.to_owned(), hold.clone());
    if let Some(v) = hold.pinned_value {
        set_brake_value(&mut state.tuning, brake, v);
    }
    let entry = audited_entry(
        &hold.actor,
        "hold",
        brake,
        from,
        to,
        &hold.reason,
        Some(hold.expires_at),
    );
    state.record_tuning_change(entry);
    true
}

/// One brake card in the cockpit read model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrakeCard {
    /// Stable card id (`quality` / `intake`).
    pub id: String,
    /// The `Tuning` field this brake drives (`bugs_first` / `skip_ba`).
    pub field_name: String,
    /// The value consumers read today (holds already applied).
    pub effective_value: bool,
    /// `auto` | `held-freeze` | `overridden`.
    pub mode: String,
    /// What the autonomous policy wants this pass.
    pub auto_would_be: bool,
}

/// The eval signals + thresholds that produced the cards.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrakeInputs {
    pub churn_per_ship: Option<f64>,
    pub shipped_last7_days: usize,
    pub backlog: usize,
}

/// A hold as the cockpit lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrakeHoldView {
    pub brake: String,
    /// `null` = freeze at the current value; `bool` = pinned override.
    pub pinned_value: Option<bool>,
    pub reason: String,
    pub actor: String,
    pub at: String,
    pub expires_at: String,
}

/// The published hysteresis bars (`decide_tuning`'s constants as data).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrakeThresholds {
    pub churn_on: f64,
    pub churn_off: f64,
    pub backlog_on: usize,
    pub backlog_off: usize,
}

/// The cockpit read model (CXA-F238 AC1): each brake's effective state, the
/// exact eval signals and hysteresis thresholds that produced it, what the
/// loop would do this pass, and every active operator hold. Computed purely
/// by applying `decide_tuning` to the real project state at request time —
/// nothing brake-related is snapshotted anywhere else.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrakeCockpit {
    /// RFC3339 moment this read model was computed.
    pub as_of: String,
    /// The day (`YYYY-MM-DD`) the persisted tuning was last evaluated.
    pub eval_day: String,
    pub inputs: BrakeInputs,
    pub cards: Vec<BrakeCard>,
    pub active_holds: Vec<BrakeHoldView>,
    pub thresholds: BrakeThresholds,
    /// The most recent audit entries, newest first — the trail a viewer can
    /// inspect (CXA-F238 AC3), capped at 50 per read.
    pub history: Vec<TuningAuditEntry>,
}

/// Build the cockpit read model over `state`. Pure.
#[must_use]
pub fn brake_cockpit(state: &ProjectState, now: &str) -> BrakeCockpit {
    let evals = agent_evals(state);
    let backlog = brake_backlog(state);
    let today = now.get(..10).unwrap_or(now).to_owned();
    let delta = compute_burndown(state, &today, BURNDOWN_WINDOW_DAYS).delta_24h;
    let raw = decide_tuning(&evals, backlog, delta, &state.tuning);
    let (active, _) = split_expired_holds(&state.tuning_overrides, now);
    let cards: Vec<BrakeCard> = BRAKES
        .iter()
        .map(|(id, field)| {
            let hold = active.get(*field);
            BrakeCard {
                id: (*id).to_owned(),
                field_name: (*field).to_owned(),
                effective_value: brake_value(&state.tuning, field),
                mode: match hold {
                    Some(h) if h.pinned_value.is_some() => "overridden".to_owned(),
                    Some(_) => "held-freeze".to_owned(),
                    None => "auto".to_owned(),
                },
                auto_would_be: brake_value(&raw, field),
            }
        })
        .collect();
    let active_holds: Vec<BrakeHoldView> = active
        .iter()
        .map(|(brake, hold)| BrakeHoldView {
            brake: brake.clone(),
            pinned_value: hold.pinned_value,
            reason: hold.reason.clone(),
            actor: hold.actor.clone(),
            at: hold.at.clone(),
            expires_at: hold.expires_at.clone(),
        })
        .collect();
    BrakeCockpit {
        as_of: now.to_owned(),
        eval_day: state.tuning.last_eval_day.clone(),
        inputs: BrakeInputs {
            churn_per_ship: Some(evals.churn_per_ship),
            shipped_last7_days: evals.shipped_7d,
            backlog,
        },
        cards,
        active_holds,
        thresholds: BrakeThresholds {
            churn_on: CHURN_BRAKE_ON,
            churn_off: CHURN_BRAKE_OFF,
            backlog_on: BACKLOG_BRAKE_ON,
            backlog_off: BACKLOG_BRAKE_OFF,
        },
        history: state
            .tuning_history
            .iter()
            .rev()
            .take(COCKPIT_HISTORY_SHOWN)
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hold(pinned: Option<bool>, expires_at: &str) -> BrakeHold {
        BrakeHold {
            pinned_value: pinned,
            reason: "r".to_owned(),
            actor: "op".to_owned(),
            at: "2026-08-01T00:00:00Z".to_owned(),
            expires_at: expires_at.to_owned(),
        }
    }

    #[test]
    fn unparsable_expiry_fails_closed_toward_autonomy() {
        let mut holds = BTreeMap::new();
        holds.insert("bugs_first".to_owned(), hold(Some(true), "not-a-time"));
        let (active, expired) = split_expired_holds(&holds, "2026-08-29T00:00:00Z");
        assert!(active.is_empty() && expired.contains_key("bugs_first"));
    }

    #[test]
    fn holds_expire_inclusive_of_their_bound() {
        let mut holds = BTreeMap::new();
        holds.insert(
            "skip_ba".to_owned(),
            hold(None, "2026-08-29T00:00:00Z"),
        );
        let (active, _) =
            split_expired_holds(&holds, "2026-08-29T00:00:00Z");
        assert!(active.is_empty(), "bound == now is expired");
    }
}
