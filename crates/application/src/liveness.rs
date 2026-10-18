//! Loop-liveness watchdog predicate (CXA-F259) — a pure verdict over a
//! snapshot, the same snapshot->pure-decision pattern as `use_cases/run_dev`
//! gates: the hub-side checker (presentation) assembles [`LivenessSnapshot`]
//! from one store read, [`stall_verdict`] decides, and the checker acts on the
//! verdict by raising the alert through the EXISTING channels — the
//! `NotifyEvent`/`NotifierPort` machinery (chat feed + webhook) every other
//! loop alert already uses. No IO here — that split is what makes every
//! acceptance criterion testable with a struct literal and no live processes.
//!
//! # The stall trigger
//!
//! A running loop (`desired_run` on) whose activity trail has had no new
//! entry for longer than the configured [`LivenessSnapshot::threshold_secs`]
//! is stalled. The ACTIVITY TRAIL governs the trigger — deliberately, per the
//! registry-TTL question: `StateStorePort::workers()` prunes heartbeats older
//! than the worker TTL, so a worker whose process died hours ago is ABSENT
//! from the live registry and its staleness is unobservable there. Silence on
//! the persisted trail is the one signal that survives every failure mode the
//! ticket names (hung cycle, dead worker process, hub restart). The worker
//! registry shapes the alert text but never gates it: a hung cycle keeps
//! beating its registry heartbeat (the keepalive task is a sibling of the
//! awaited cycle), so a fresh heartbeat says nothing about progress, and an
//! empty registry is named as "no live worker" in the alert.
//!
//! # The documented non-stall exclusions
//!
//! Silence alone is not a stall. A loop that stopped for a reason somebody
//! was already told about must never alert:
//!
//! * **desired-run off** — `desired_run`: nobody asked the loop to run. A
//!   user's Stop lands here as well as Pause (the runner handle reports
//!   stopped as not-running), so a deliberately stopped team is never a stall;
//! * **user pause** — `paused`: a person paused the loop. The budget cap
//!   ("loop paused: spend cap reached") and the engine circuit breaker pause
//!   this same runner handle, and each posted its own alert at the moment it
//!   did, so they arrive through the same flag and are excluded with it;
//! * **budget cap** — `over_budget`: spend reached the lifetime or daily
//!   cap; the loop announced `loop_paused`;
//! * **quota exhaustion** — `quota_exhausted`: the trail's newest entry is
//!   the quota pause line ([`QUOTA_PAUSE_LINE`], posted by the cycle when
//!   the engines run out) and no newer activity has landed since;
//! * **empty backlog** — `backlog_empty`: nothing actionable exists, so an
//!   idle team is correct, not stalled.
//!
//! # Deliberate bad-data stance
//!
//! Unparseable timestamps NEVER produce a verdict: a trail stamp or `now`
//! that does not parse leaves the loop `Quiet` (and an open episode
//! `Resumed`) — guessing in either direction would fabricate or hide stalls.
//!
//! # Episode lifecycle (dedupe and self-clear)
//!
//! The verdict is a function of the already-open episode too: the first
//! stall of an episode returns [`LivenessVerdict::Stall`]; the same episode
//! never alerts twice while it stays open (dedupe — a multi-hour stall is
//! one alert, not one per sweep); past
//! [`LivenessSnapshot::escalate_after_secs`] of unacknowledged silence it
//! returns [`LivenessVerdict::Escalate`] with the escalation counter
//! advanced; and the moment new activity lands (or a documented non-stall
//! reason takes over) the open episode returns
//! [`LivenessVerdict::Resumed`] — the condition self-clears until activity
//! resumes, and a later stall is a fresh episode.

use crate::config::WorkflowConfig;
use crate::ports::outbound::WorkerEntry;
use crate::state::{ActivityEntry, StallEpisode};
use coxagent_domain::{Status, Ticket};

/// The effective stall-silence threshold: the configured
/// `workflow.stall_timeout_secs`, or the built-in default when unset/zero
/// (the `CadenceConfig` zero-means-default precedent).
#[must_use]
pub fn stall_threshold(cfg: &WorkflowConfig) -> u64 {
    if cfg.stall_timeout_secs == 0 {
        3600
    } else {
        cfg.stall_timeout_secs
    }
}

/// The effective escalation horizon for an already-alerted stall: the
/// configured `workflow.stall_hub_timeout_secs`, or the built-in default
/// when unset/zero.
#[must_use]
pub fn stall_escalation(cfg: &WorkflowConfig) -> u64 {
    if cfg.stall_hub_timeout_secs == 0 {
        14_400
    } else {
        cfg.stall_hub_timeout_secs
    }
}

/// The activity-trail line the cycle posts when the engines run out of quota
/// (`use_cases/cycle/mod.rs`). While it is the trail's newest entry, silence
/// is explained: the loop told the operator and is waiting, not stuck.
pub const QUOTA_PAUSE_LINE: &str = "paused — engines out of quota";

/// Everything the stall verdict is a pure function of, assembled hub-side
/// from one store read (worker registry + activity trail + open episode).
// Independent predicate inputs, not a state machine — each bool is one
// documented trigger or exclusion (see the module docs).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct LivenessSnapshot {
    /// The freshest worker-registry entry (`StateStorePort::workers()` head)
    /// — `None` when no live worker remains (the store prunes stale
    /// heartbeats, so an empty registry means the worker process is gone).
    pub worker: Option<WorkerEntry>,
    /// The activity trail, newest last — its tail is "the last activity".
    pub activity: Vec<ActivityEntry>,
    /// Desired-run is on: a Start is in effect and nothing has stopped or
    /// paused the loop since.
    pub desired_run: bool,
    /// A pause is in effect (a person's Pause; the budget cap and the engine
    /// breaker pause the same handle and already announced themselves).
    pub paused: bool,
    /// A budget cap is binding right now (lifetime or daily spend reached).
    pub over_budget: bool,
    /// The engines are out of quota — the trail's newest entry is
    /// [`QUOTA_PAUSE_LINE`] and nothing newer has landed.
    pub quota_exhausted: bool,
    /// The actionable backlog is empty — an idle team is correct.
    pub backlog_empty: bool,
    /// The configured silence threshold in seconds; a trail silent strictly
    /// longer than this is stalled.
    pub threshold_secs: i64,
    /// How long an already-alerted episode may stay quiet before one
    /// escalation re-alert (seconds).
    pub escalate_after_secs: i64,
    /// The already-open stall episode, if one is being tracked — the dedupe
    /// key that keeps a multi-hour stall at one alert.
    pub open_stall: Option<StallEpisode>,
    /// Now, RFC3339 — injected so the verdict stays a pure function.
    pub now: String,
}

/// One alert to raise: the operator-facing text and the episode record to
/// persist with it (the next sweep dedupes against that record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StallAlert {
    /// One line naming the worker, the last activity and the elapsed silence.
    pub message: String,
    /// The episode state to persist for this alert (open or escalated).
    pub episode: StallEpisode,
}

/// What the checker should do after one sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivenessVerdict {
    /// Nothing to do: healthy, a documented non-stall reason is in effect,
    /// or the open episode already alerted inside its escalation window.
    Quiet,
    /// First alert of a new stall episode — raise it and persist the episode.
    Stall(StallAlert),
    /// The open episode is still stalled past the escalation horizon —
    /// re-alert once and advance the episode's escalation counter.
    Escalate(StallAlert),
    /// New activity landed (or a documented non-stall reason took over) while
    /// an episode was open — notify the recovery and clear the episode.
    Resumed,
}

/// Whether any ticket is actionable: a loop with work it could pick up is
/// expected to leave traces; a loop with none may idle silently forever.
/// Terminal and deliberately-parked statuses do not count as actionable.
#[must_use]
pub fn backlog_is_empty(tickets: &[Ticket]) -> bool {
    !tickets.iter().any(|t| {
        matches!(
            t.status(),
            Status::Pending | Status::Ready | Status::InProgress | Status::Open
        )
    })
}

/// Seconds between two RFC3339 instants (`t.unix_timestamp()`), or `None`
/// when the stamp does not parse. Shared by the predicate and the hub-side
/// checker (which must order worker heartbeats by INSTANT, not by string —
/// store-RPC heartbeats may carry non-UTC offsets whose string order lies).
#[must_use]
pub fn rfc3339_secs(s: &str) -> Option<i64> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(time::OffsetDateTime::unix_timestamp)
}

/// The stall verdict: trigger, alert text and clear condition in one pure
/// function over the snapshot (AC4). See the module docs for the trigger and
/// the documented non-stall exclusions.
#[must_use]
pub fn stall_verdict(snap: &LivenessSnapshot) -> LivenessVerdict {
    // A resolved condition (documented non-stall reason, fresh activity, or
    // nothing to measure) clears an open episode and is otherwise quiet.
    let settled = |snap: &LivenessSnapshot| {
        if snap.open_stall.is_some() {
            LivenessVerdict::Resumed
        } else {
            LivenessVerdict::Quiet
        }
    };

    // ---- documented non-stall reasons: never a stall ----------------------
    if !snap.desired_run {
        return settled(snap); // nobody asked the loop to run (Stop lands here too)
    }
    if snap.paused {
        return settled(snap); // user pause (budget cap / engine breaker pause the same handle)
    }
    if snap.over_budget {
        return settled(snap); // budget cap — loop_paused already told the operator
    }
    if snap.quota_exhausted {
        return settled(snap); // quota exhaustion — quota_exhausted already told
    }
    if snap.backlog_empty {
        return settled(snap); // empty backlog — an idle team is correct
    }

    // ---- silence measurement ----------------------------------------------
    let Some(last) = snap.activity.last() else {
        // An empty trail has no "last activity" to measure from; the watchdog
        // arms once the first entry lands.
        return settled(snap);
    };
    let (Some(last_at), Some(now_at)) = (rfc3339_secs(&last.at), rfc3339_secs(&snap.now)) else {
        // Unparseable timestamps: never guess a stall from bad data (the
        // deliberate stance — see the module docs).
        return settled(snap);
    };
    let silent = now_at - last_at;
    if silent <= snap.threshold_secs {
        // Activity is recent — a previously-open episode clears here: the
        // condition self-clears the moment new activity lands.
        return settled(snap);
    }

    // ---- stall: build the alert naming worker, last activity, silence -----
    let last_text = format!("\"{}: {}\"", last.agent, last.action);
    let since = snap.now.clone();
    let worker_label = match &snap.worker {
        Some(w) => w.worker.clone(),
        None => "none — no live worker left in the registry (the worker process \
                 is gone or its heartbeat expired)"
            .to_owned(),
    };
    let mut message = format!(
        "LOOP STALLED: worker {worker_label} — silent for {} with no new activity \
         for that whole time; last activity {last_text} ({} ago). The cycle looks \
         stuck: check the worker process, the engine calls and the store locks. A \
         single engine call longer than workflow.stall_timeout_secs also reads as \
         silence — raise the knob if your runs are longer.",
        humanize(silent),
        humanize(silent),
    );
    let mut episode = StallEpisode {
        since,
        worker: snap
            .worker
            .as_ref()
            .map_or_else(|| "none".to_owned(), |w| w.worker.clone()),
        last_activity_at: last.at.clone(),
        last_alert_at: snap.now.clone(),
        escalations: 0,
    };
    match &snap.open_stall {
        None => LivenessVerdict::Stall(StallAlert { message, episode }),
        Some(open) => {
            // Dedupe: this episode already alerted. Hold the alert until
            // activity resumes — unless the silence has outlasted the
            // escalation horizon, then re-alert once with the counter.
            let since_alert = rfc3339_secs(&open.last_alert_at).map_or(i64::MAX, |a| now_at - a);
            if since_alert < snap.escalate_after_secs {
                return LivenessVerdict::Quiet;
            }
            episode.since.clone_from(&open.since);
            episode.escalations = open.escalations.saturating_add(1);
            message = format!("ESCALATION {}: {message}", episode.escalations);
            LivenessVerdict::Escalate(StallAlert { message, episode })
        }
    }
}

/// Compact human duration for alert text: `2 h 5 m`, `45 m`, `59 s`.
fn humanize(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 3600 {
        format!("{} h {} m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{} m", s / 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod liveness_tests {
    use super::*;
    use crate::state::now_rfc3339;
    use coxagent_domain::{Role, TicketType};

    fn ago(secs_ago: i64) -> String {
        (time::OffsetDateTime::now_utc() - time::Duration::seconds(secs_ago))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("rfc3339")
    }

    fn worker(at_secs_ago: i64) -> WorkerEntry {
        WorkerEntry {
            worker: "luton@macbook".to_owned(),
            role: "DEV-FEATURE".to_owned(),
            ticket: "CXC-259".to_owned(),
            at: ago(at_secs_ago),
            engines: vec!["claude".to_owned()],
            models: Vec::new(),
            git: None,
            tooling: None,
            version: String::new(),
        }
    }

    fn activity(at_secs_ago: i64, action: &str) -> ActivityEntry {
        ActivityEntry {
            at: ago(at_secs_ago),
            agent: "SM".to_owned(),
            action: action.to_owned(),
            ticket: None,
        }
    }

    /// Healthy base snapshot: running loop, live worker, activity 5 min ago,
    /// one-hour threshold.
    fn snap() -> LivenessSnapshot {
        LivenessSnapshot {
            worker: Some(worker(30)),
            activity: vec![activity(300, "daily digest posted")],
            desired_run: true,
            paused: false,
            over_budget: false,
            quota_exhausted: false,
            backlog_empty: false,
            threshold_secs: 3600,
            escalate_after_secs: 14_400,
            open_stall: None,
            now: now_rfc3339(),
        }
    }

    #[test]
    fn a_running_loop_with_recent_activity_is_quiet() {
        assert_eq!(stall_verdict(&snap()), LivenessVerdict::Quiet);
    }

    #[test]
    fn silence_past_the_threshold_stalls_and_names_worker_last_activity_and_silence() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "fixed login 500")];
        let v = stall_verdict(&s);
        let LivenessVerdict::Stall(a) = v else {
            panic!("expected a stall, got {v:?}");
        };
        assert!(a.message.contains("worker"), "{}", a.message);
        assert!(a.message.contains("last activity"), "{}", a.message);
        assert!(a.message.contains("silent for"), "{}", a.message);
        assert!(a.message.contains("fixed login 500"), "{}", a.message);
        assert_eq!(a.episode.escalations, 0);
        assert_eq!(a.episode.worker, "luton@macbook");
    }

    #[test]
    fn a_worker_whose_process_is_gone_empty_registry_still_stalls() {
        // The store prunes stale heartbeats: a dead worker is an ABSENT
        // registry entry, and the hub-side checker must still fire — the
        // trail governs the trigger, the registry only shapes the text.
        let mut s = snap();
        s.worker = None;
        s.activity = vec![activity(7_200, "fixed login 500")];
        let LivenessVerdict::Stall(a) = stall_verdict(&s) else {
            panic!("a gone worker with silent activity is a stall");
        };
        assert!(
            a.message.contains("no live worker"),
            "the alert must say the worker is gone: {}",
            a.message
        );
    }

    #[test]
    fn silence_exactly_at_the_threshold_is_not_yet_a_stall() {
        // AC1: "no new entry for LONGER THAN the configured threshold".
        let mut s = snap();
        s.activity = vec![activity(3_600, "digest")];
        s.now = ago(0);
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn a_user_pause_is_a_documented_non_stall_reason() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "digest")];
        s.paused = true;
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn desired_run_off_is_a_documented_non_stall_reason() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "digest")];
        s.desired_run = false;
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn a_budget_cap_is_a_documented_non_stall_reason() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "digest")];
        s.over_budget = true;
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn quota_exhaustion_is_a_documented_non_stall_reason() {
        let mut s = snap();
        s.activity = vec![activity(7_200, QUOTA_PAUSE_LINE)];
        s.quota_exhausted = true;
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn an_empty_backlog_is_a_documented_non_stall_reason() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "digest")];
        s.backlog_empty = true;
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn an_empty_trail_has_no_last_activity_to_measure_and_is_quiet() {
        let mut s = snap();
        s.activity.clear();
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn an_open_episode_never_alerts_twice_inside_the_escalation_window() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "digest")];
        let LivenessVerdict::Stall(first) = stall_verdict(&s) else {
            panic!("first sighting stalls");
        };
        // Same episode, ten minutes later, no new activity: deduped.
        s.open_stall = Some(first.episode.clone());
        s.now = ago(-600); // 10 min after the first alert
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn an_open_episode_escalates_once_past_the_horizon_and_counts() {
        let mut s = snap();
        s.activity = vec![activity(20_000, "digest")];
        let LivenessVerdict::Stall(first) = stall_verdict(&s) else {
            panic!("first sighting stalls");
        };
        // Five hours after the first alert (past the 4 h escalation horizon):
        // one re-alert, counter advanced, episode since preserved.
        s.open_stall = Some(first.episode.clone());
        s.now = ago(-18_000);
        let LivenessVerdict::Escalate(a) = stall_verdict(&s) else {
            panic!("expected an escalation, got {:?}", stall_verdict(&s));
        };
        assert_eq!(a.episode.escalations, 1);
        assert_eq!(a.episode.since, first.episode.since);
        assert!(a.message.contains("ESCALATION 1"), "{}", a.message);
    }

    #[test]
    fn new_activity_landing_clears_the_open_episode_and_reports_the_resume() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "digest")];
        let LivenessVerdict::Stall(first) = stall_verdict(&s) else {
            panic!("first sighting stalls");
        };
        // Activity resumes: the episode self-clears.
        s.open_stall = Some(first.episode);
        s.activity.push(activity(5, "implemented the fix"));
        assert_eq!(stall_verdict(&s), LivenessVerdict::Resumed);
    }

    #[test]
    fn a_documented_non_stall_reason_taking_over_also_clears_the_open_episode() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "digest")];
        let LivenessVerdict::Stall(first) = stall_verdict(&s) else {
            panic!("first sighting stalls");
        };
        s.open_stall = Some(first.episode);
        s.paused = true; // the operator paused while the alert was open
        assert_eq!(stall_verdict(&s), LivenessVerdict::Resumed);
    }

    #[test]
    fn unparseable_timestamps_never_guess_a_stall() {
        let mut s = snap();
        s.activity = vec![activity(7_200, "digest")];
        s.now = "not a timestamp".to_owned();
        assert_eq!(stall_verdict(&s), LivenessVerdict::Quiet);
    }

    #[test]
    fn backlog_emptiness_follows_the_actionable_statuses() {
        let ticket = |kind: TicketType| {
            Ticket::new(
                coxagent_domain::TicketId::new("CXC-1".to_owned()).expect("id"),
                kind,
                "demo",
                "demo ticket",
                coxagent_domain::Priority::Medium,
                coxagent_domain::Complexity::Small,
                false,
            )
            .expect("valid ticket")
        };
        // A fresh feature (Pending) and a fresh bug (Open) are actionable.
        assert!(!backlog_is_empty(&[ticket(TicketType::Feature)]));
        assert!(!backlog_is_empty(&[ticket(TicketType::Bug)]));
        assert!(backlog_is_empty(&[]), "no tickets at all — nothing to do");
        // Parked work is deliberately out of play — not actionable.
        let mut parked = ticket(TicketType::Feature);
        parked
            .transition_to(Role::System, Status::OnHold)
            .expect("legal transition");
        assert!(
            backlog_is_empty(&[parked]),
            "a parked backlog has nothing actionable"
        );
    }

    #[test]
    fn rfc3339_secs_compares_instants_across_utc_offsets() {
        // Same instant, two spellings: the +02:00 form string-sorts LATER
        // than the Z form but is the SAME instant.
        let z = "2026-08-31T10:00:00Z";
        let plus_two = "2026-08-31T12:00:00+02:00";
        assert_eq!(rfc3339_secs(z), rfc3339_secs(plus_two));
        assert!(rfc3339_secs("garbage").is_none());
        // And an older instant written with a positive offset string-sorts
        // after a fresher Z stamp — the exact lie string ordering tells.
        let older_offset = "2026-08-31T11:00:00+02:00"; // = 09:00Z, OLDER
        assert!(
            older_offset > z,
            "precondition: the older instant string-sorts later"
        );
        assert!(rfc3339_secs(older_offset) < rfc3339_secs(z));
    }
}
