//! Daily sweep RUNNER (CXA-F404 [2/3], part 2 of CXA-F376) — wires part 1's
//! pure [`crate::use_cases::daily_recovery`] core into one deterministic,
//! once-per-UTC-day pass over auto-held tickets.
//!
//! Loop-evasion discipline (sprints 647–652 lessons): the sweep OBSERVES and
//! reports — it enqueues no cycles and starts no work; resumption stays owned
//! by the existing escalation ladder. It flips an eligible held bug to
//! `Pending`, stamps exactly ONE diagnosis record per ticket per day
//! (`SweepDiagnosis`, overwritten same-day), notifies once, and returns.
//! Failure isolation: a per-ticket error is logged and skipped; a store
//! failure never fails the cycle.
//!
//! IDEMPOTENCY — the claim IS the persisted day record:
//! [`claim_sweep_day`] mutates `sweep_days[day]` in one guarded save —
//! absent → insert Running and win; Running but stale (>30 min) → reclaim
//! (`reclaimed: true`) and win; Running fresh or Complete → lose. Atomic
//! under the store's optimistic concurrency, so N runners run it exactly
//! once with no crash window between "claimed" and "recorded". Because the
//! per-ticket diagnosis carries the day key and is OVERWRITTEN (never
//! appended), a re-run recomputes the identical verdict — duplicates are
//! impossible by construction.

use std::sync::Arc;

use crate::ports::outbound::notify::{NotifyEvent, NotifierPort};
use crate::ports::outbound::state_store::{mutate_state, StateStorePort};
use crate::state::diagnosis::SweepDiagnosis;
use crate::state::{now_rfc3339, ProjectState, SweepDay, SweepPhase};
use crate::use_cases::daily_recovery::{
    recovery_eligibility, RecoverySnapshot, SWEEP_ACTOR,
};

/// How long a `Running` day claim may sit before another runner reclaims it
/// (crash-recovery window). Seconds past midnight parsed from the stamp.
const RECLAIM_AFTER_SECS: u64 = 30 * 60;

/// Retention for the day ledger — older days are pruned each run, mirroring
/// `prune_archives`.
const DAY_RETENTION: usize = 90;

/// The daily sweep entry point — idempotent, once per UTC day. Called from
/// the leader-only pass sequence in `run_cycle`; a failure here must NEVER
/// fail the cycle, so every error path warns and returns quietly.
pub(super) async fn daily_sweep<S: StateStorePort>(
    store: &Arc<S>,
    notifier: Option<&dyn NotifierPort>,
    dry_run: bool,
) {
    let day = today();
    // 1. Once-per-day claim — atomic; the record itself is lock AND ledger.
    let claim = match claim_sweep_day(store, &day, dry_run).await {
        Ok(c) => c,
        Err(e) => return tracing::warn!("daily-sweep({day}): claim failed: {e}"),
    };
    if !claim.won {
        tracing::info!(
            "daily-sweep({day}): not won (phase {:?}); skipping",
            claim.phase
        );
        return;
    }

    // 2. Select candidates and decide — PURE call over snapshots (part 1).
    let (subjects, actions, kept_causes) =
        match select_and_decide(store, &day).await {
            Ok(p) => p,
            Err(e) => return tracing::warn!("daily-sweep({day}): selection failed: {e}"),
        };

    if dry_run {
        // Zero writes: log exactly what a real pass would do, then leave the
        // day ledger untouched so tomorrow's real run still happens.
        tracing::info!(
            "daily-sweep({day}) DRY-RUN: subjects={subjects}; {}",
            kept_causes.join(" | ")
        );
        return;
    }

    // 3. Apply verdicts one per-ticket guarded save — failures isolated.
    let mut resumed = 0usize;
    let mut kept = kept_causes.len();
    for (ticket, hold_reason, detail) in actions {
        match apply_resume(store, &day, &ticket, &hold_reason, &detail).await {
            Ok(()) => resumed += 1,
            Err(e) => {
                tracing::warn!("daily-sweep({day}): ticket {ticket}: skipped: {e}");
            }
        }
    }

    // 4. Close the day ledger (Complete) with final counts, prune retention.
    let hint = (subjects == 0)
        .then(|| "nothing held — nothing to diagnose".to_owned());
    if let Err(e) =
        complete_sweep_day(store, &day, subjects, resumed, kept, hint.as_deref()).await
    {
        tracing::warn!("daily-sweep({day}): could not close the day ledger: {e}");
    }

    tracing::info!(
        "daily-sweep({day}): subjects={subjects} resumed={resumed} kept_held={kept}"
    );

    // 5. One digest notification per day.
    if let Some(n) = notifier {
        let summary = if subjects == 0 {
            format!("Daily sweep {day}: nothing held — nothing to diagnose.")
        } else {
            format!(
                "Daily sweep {day}: {subjects} held ticket(s) examined, \
                 {resumed} resumed, {kept} kept held."
            )
        };
        n.notify(&NotifyEvent {
            kind: "daily_sweep".into(),
            message: summary,
            project: None,
            channel: None,
        })
        .await;
    }
}

// ------------------------------------------------------------------ clock ----

/// UTC day key (`YYYY-MM-DD`) without pulling chrono into the application
/// layer — the RFC3339 stamp already starts with the date.
fn today() -> String {
    now_rfc3339()[..10].to_owned()
}

/// Seconds since that day's 00:00 from an RFC3339 stamp — enough fidelity for
/// the 30-minute reclaim window; claims across midnight land on a different
/// day key anyway, so the missing date arithmetic cannot misfire.
fn secs_past_midnight(stamp: &str) -> u64 {
    let t = stamp.find('T').map_or(11, |t| t + 1);
    let b = stamp.as_bytes();
    let d = |i: usize| u64::from(b.get(i).copied().unwrap_or(b'0').saturating_sub(b'0'));
    let hh = d(t) * 10 + d(t + 1);
    let mm = d(t + 3) * 10 + d(t + 4);
    let ss = d(t + 6) * 10 + d(t + 7);
    hh * 3600 + mm * 60 + ss
}

// ------------------------------------------------------------------ claim ----

struct ClaimOutcome {
    won: bool,
    phase: Option<SweepPhase>,
}

/// PURE decision over what the store showed us: does this runner take the day?
/// (won, reclaimed) — invariant-tested without any store.
fn claim_decision(seen: Option<&SweepDay>, now_secs_past_midnight: u64) -> (bool, bool) {
    match seen {
        None => (true, false),
        Some(d) if d.phase == SweepPhase::Complete => (false, false),
        Some(d) if d.phase == SweepPhase::Running => {
            let age = now_secs_past_midnight.saturating_sub(secs_past_midnight(&d.at));
            (age >= RECLAIM_AFTER_SECS, age >= RECLAIM_AFTER_SECS)
        }
        Some(_) => (false, false),
    }
}

async fn claim_sweep_day<S: StateStorePort>(
    store: &Arc<S>,
    day: &str,
    dry_run: bool,
) -> Result<ClaimOutcome, crate::ports::outbound::PortError> {
    let at = now_rfc3339();
    mutate_state(store, |s: &mut ProjectState| {
        let now_secs = secs_past_midnight(&at);
        let (won, reclaimed) =
            claim_decision(s.sweep_days.get(day), now_secs);
        if !won || dry_run {
            let phase = s.sweep_days.get(day).map(|d| d.phase);
            return Ok(ClaimOutcome { won, phase });
        }
        s.sweep_days.insert(
            day.to_owned(),
            SweepDay {
                day: day.to_owned(),
                phase: SweepPhase::Running,
                subjects: 0,
                recovered: 0,
                escalated: 0,
                at: at.clone(),
                by: SWEEP_ACTOR.to_owned(),
                reclaimed,
                hint: String::new(),
            },
        );
        prune_old_days(s, day);
        Ok(ClaimOutcome { won: true, phase: Some(SweepPhase::Running) })
    })
    .await
}

/// Keep the bounded day ledger: drop entries older than `DAY_RETENTION`
/// (days sort lexicographically as YYYY-MM-DD, so retain_from works).
fn prune_old_days(state: &mut ProjectState, today: &str) {
    let cutoff = today[..8].to_string()
        + &format!("{:02}", (today[8..10].parse::<u32>().unwrap_or(0)))
            .rsplit_once(char::is_whitespace)
            .map_or(String::new(), |(_, r)| r.to_string());
    let _ = cutoff; // simple bounded prune below instead
    while state.sweep_days.len() > DAY_RETENTION {
        let oldest = match state.sweep_days.keys().next().cloned() {
            Some(k) if k < today => k,
            _ => break,
        };
        state.sweep_days.remove(&oldest);
    }
}

async fn complete_sweep_day<S: StateStorePort>(
    store: &Arc<S>,
    day: &str,
    subjects: usize,
    recovered: usize,
    escalated: usize,
    hint: Option<&str>,
) -> Result<(), crate::ports::outbound::PortError> {
    let at = now_rfc3339();
    mutate_state(store, |s: &mut ProjectState| {
        if let Some(d) = s.sweep_days.get_mut(day) {
            if d.phase != SweepPhase::Running {
                return Ok(()); // someone else owns/closed the day — don't fight it
            }
            d.phase = SweepPhase::Complete;
            d.subjects = subjects;
            d.recovered = recovered;
            d.escalated = escalated;
            d.hint = hint.unwrap_or_default().to_owned();
            d.at = at.clone();
        }
        Ok(())
    })
    .await
}

// -------------------------------------------------------- decide + apply ----

type Decision = (String /*ticket*/, String /*hold*/, String /*detail*/);

/// Load the aggregate, build part-1 snapshots for every auto-held bug, and
/// run the PURE eligibility decision. Returns (subjects, resume-actions,
/// keep-causes). No writes happen here — the caller applies them.
async fn select_and_decide<S: StateStorePort>(
    store: &Arc<S>,
    day: &str,
) -> Result<(usize, Vec<Decision>, Vec<String>), crate::ports::outbound::PortError> {
    let state = store.load().await?;
    let at = now_rfc3339();
    let mut actions = Vec::new();
    let mut kept = Vec::new();

    let ids: Vec<String> = state
        .tickets
        .iter()
        .filter(|t| t.status == coxagent_domain::Status::OnHold && t.kind.is_bug())
        .map(|t| t.id.alias_str().to_owned())
        .collect();
    for id in &ids {
        let Some(snapshot) = snapshot_for(&state, id) else { continue };
        let v = recovery_eligibility(&snapshot, &at);
        let hold = snapshot
            .hold_reason
            .clone()
            .unwrap_or_else(|| "auto-held after rescue budget spent".to_owned());
        if v.eligible {
            actions.push((id.clone(), hold, v.diagnosis));
        } else {
            kept.push(format!("{}: {}", id, v.diagnosis));
        }
    }
    let _ = day;
    Ok((ids.len(), actions, kept))
}

/// Build part-1's snapshot struct from the loaded aggregate (read-only, no IO).
fn snapshot_for(state: &ProjectState, alias: &str) -> Option<RecoverySnapshot> {
    let ticket = state.tickets.iter().find(|t| t.id.alias_str() == alias)?;
    Some(RecoverySnapshot {
        id: alias.to_owned(),
        ticket_type: ticket.kind,
        status: ticket.status,
        rescues_used: state.ticket_redesigns.get(alias).copied().unwrap_or(0),
        attempts: state
            .ticket_failures
            .get(alias)
            .cloned()
            .unwrap_or_default(),
        hold_reason: state.hold_reasons.get(alias).cloned(),
        live_parent: ticket.parent_id.as_ref().is_some_and(|p| {
            state
                .tickets
                .iter()
                .any(|pt| pt.id == *p && !matches!(pt.status, coxagent_domain::Status::Done | coxagent_domain::Status::Documented | coxagent_domain::Status::Rejected))
        }),
    })
}

/// Persist one verdict through a guarded save. Re-checks the ticket's status
/// INSIDE the transaction (write-time re-check): if something already resumed
/// it since our read, we skip — a second sweep can never double-resume or
/// double-append because the diagnosis row carries the day key and is
/// overwritten, making same-day repeats byte-identical no-ops.
async fn apply_resume<S: StateStorePort>(
    store: &Arc<S>,
    day: &str,
    ticket: &str,
    hold_reason: &str,
    detail: &str,
) -> Result<bool, crate::ports::outbound::PortError> {
    let at = now_rfc3339();
    let record = SweepDiagnosis {
        ticket: ticket.to_owned(),
        day: day.to_owned(),
        verdict: "resumed".to_owned(),
        hold_reason: hold_reason.to_owned(),
        detail: detail.to_owned(),
        at: at.clone(),
        by: SWEEP_ACTOR.to_owned(),
    };
    mutate_state(store, |s: &mut ProjectState| {
        // Write-time re-check against concurrent transitions.
        let Some(t) = s
            .tickets
            .iter_mut()
            .find(|t| t.id.alias_str() == ticket)
        else {
            return Ok(false);
        };
        if t.status != coxagent_domain::Status::OnHold {
            return Ok(false); // already moved on — nothing to do, no duplicate
        }
        if t.transition_to(coxagent_domain::Role::System, coxagent_domain::Status::Pending)
            .is_err()
        {
            return Ok(false);
        }
        if let Some(prev) = s.sweep_diagnoses.get(ticket) {
            if prev.day == day && prev.detail == detail {
                return Ok(false); // same-day identical re-diagnose: no-op
            }
        }
        s.sweep_diagnoses.insert(ticket.to_owned(), record);
        // Attributed activity line — the dashboard-visible diagnosis trail.
        s.log_activity(SWEEP_ACTOR, &format!("resumed auto-held ticket — {detail}"), Some(ticket.to_owned()));
        Ok(true)
    })
    .await
}
